//! Windowed replay-to-frame-callback benchmark (`S1-13` and Stage E evidence path).
//!
//! Drives deterministic replay deltas through the real GPUI window one update per
//! frame and measures update-to-next-frame latency and callback cadence from GPUI's
//! `on_next_frame` callbacks against the display profile reported by
//! [`NativeDisplayProbe`]. Renderer submission is genuinely performed through a
//! native GPUI window. These callbacks run after the prior render but do not prove
//! physical scanout, and the report says so.

use axiusflow_application::{ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate};
use axiusflow_chart_integration::OriginChartView;
use axiusflow_coinbase_market_adapter::{CoinbaseInterval, aggregate_coinbase_bars};
use axiusflow_desktop_market_runtime::market_worker::FixtureMarketWorker;
use axiusflow_market_data::BarDefinition;
use axiusflow_platform_runtime::{DisplayOutput, NativeDisplayProbe};
#[cfg(target_os = "windows")]
use axiusflow_platform_runtime::{WindowsCompositionProbe, WindowsCompositionTiming};
use gpui::{
    App, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    size,
};
use gpui_platform::application;
use serde::Serialize;
use std::{
    cell::RefCell,
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};

const SNAPSHOT_BARS: usize = 256;
const WARMUP_FRAMES: usize = 32;
const MEASURED_FRAMES: usize = 256;

#[derive(Serialize)]
struct DisplayOutputEvidence {
    name: Option<String>,
    description: Option<String>,
    refresh_millihertz: Option<u32>,
    refresh_interval_nanos: Option<u64>,
    pixel_size: Option<(u32, u32)>,
    effective_scale_milli: Option<u32>,
}

#[derive(Serialize)]
struct DisplayEvidence {
    outputs: Vec<DisplayOutputEvidence>,
    presentation_clock: Option<String>,
    probe: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct LatencyEvidence {
    p50: u64,
    p95: u64,
    p99: u64,
    p99_9: u64,
    maximum: u64,
}

#[cfg(target_os = "windows")]
#[derive(Serialize)]
struct WindowsCompositionEvidence {
    availability: &'static str,
    timeline_observation: &'static str,
    scope: &'static str,
    semantics: &'static str,
    qpc_frequency_hz: Option<u64>,
    reported_refresh_period_nanos: Option<u64>,
    samples: usize,
    initial_flush: &'static str,
    final_flush: &'static str,
    first_vblank_qpc: Option<u64>,
    last_vblank_qpc: Option<u64>,
    first_compose_qpc: Option<u64>,
    last_compose_qpc: Option<u64>,
    first_displayed_frame: Option<u64>,
    last_displayed_frame: Option<u64>,
    displayed_frame_advances: u64,
    displayed_frame_interval: Option<LatencyEvidence>,
    first_completed_frame: Option<u64>,
    last_completed_frame: Option<u64>,
    completed_frame_advances: u64,
    refresh_advances: u64,
    refresh_interval: Option<LatencyEvidence>,
    late_frame_delta: u64,
    dropped_frame_delta: u64,
    missed_frame_delta: u64,
}

#[derive(Serialize)]
struct LatencyTargets {
    warm_first_pixel_met: bool,
    timeframe_switch_met: bool,
}

#[derive(Serialize)]
struct WindowedBenchmarkReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: Option<String>,
    display: DisplayEvidence,
    warmup_frames: usize,
    measured_frames: usize,
    updates_published: usize,
    first_pixel_nanos: u64,
    timeframe_switch_first_pixel_nanos: u64,
    latency_targets: LatencyTargets,
    update_to_frame_callback: LatencyEvidence,
    frame_callback_interval: LatencyEvidence,
    renderer_submission_performed: bool,
    compositor_presentation_evidence: &'static str,
    #[cfg(target_os = "windows")]
    windows_dwm_composition: WindowsCompositionEvidence,
    physical_presentation_measured: bool,
    presentation_measurement_method: &'static str,
    external_scanout_instrumented: bool,
    limitations: [&'static str; 5],
}

struct FrameSample {
    update_to_frame_callback_nanos: u64,
    frame_callback_interval_nanos: u64,
}

struct BenchmarkDriver {
    chart: Entity<OriginChartView>,
    worker: FixtureMarketWorker,
    timeframe_snapshot: Option<ReplaySnapshot>,
    previous_sequence: u64,
    iteration: usize,
    submitted_at: Instant,
    last_callback_at: Instant,
    samples: Vec<FrameSample>,
    report_path: PathBuf,
    opened_at: Instant,
    first_pixel_nanos: Option<u64>,
    timeframe_switch_submitted_at: Option<Instant>,
    timeframe_switch_first_pixel_nanos: Option<u64>,
    #[cfg(target_os = "windows")]
    composition_probe: Option<WindowsCompositionProbe>,
    #[cfg(target_os = "windows")]
    composition_samples: Vec<WindowsCompositionTiming>,
    #[cfg(target_os = "windows")]
    initial_composition_flush_succeeded: bool,
    #[cfg(target_os = "windows")]
    final_composition_flush_succeeded: bool,
}

enum Step {
    Continue,
    Finish,
}

impl BenchmarkDriver {
    fn step(&mut self, window: &mut Window, cx: &mut App) -> Step {
        let callback_at = Instant::now();
        self.first_pixel_nanos.get_or_insert_with(|| {
            u64::try_from(
                callback_at
                    .saturating_duration_since(self.opened_at)
                    .as_nanos(),
            )
            .unwrap_or(u64::MAX)
        });
        if let Some(submitted_at) = self.timeframe_switch_submitted_at.take() {
            self.timeframe_switch_first_pixel_nanos = Some(
                u64::try_from(
                    callback_at
                        .saturating_duration_since(submitted_at)
                        .as_nanos(),
                )
                .unwrap_or(u64::MAX),
            );
        }
        #[cfg(target_os = "windows")]
        self.sample_composition();
        if self.iteration > 0 {
            let update_to_frame_callback = callback_at
                .saturating_duration_since(self.submitted_at)
                .as_nanos();
            let frame_callback_interval = callback_at
                .saturating_duration_since(self.last_callback_at)
                .as_nanos();
            if self.iteration > WARMUP_FRAMES && self.samples.len() < MEASURED_FRAMES {
                self.samples.push(FrameSample {
                    update_to_frame_callback_nanos: u64::try_from(update_to_frame_callback)
                        .unwrap_or(u64::MAX),
                    frame_callback_interval_nanos: u64::try_from(frame_callback_interval)
                        .unwrap_or(u64::MAX),
                });
            }
        }
        self.last_callback_at = callback_at;
        if self.samples.len() >= MEASURED_FRAMES {
            if self.timeframe_switch_first_pixel_nanos.is_some() {
                return Step::Finish;
            }
            let Some(snapshot) = self.timeframe_snapshot.take() else {
                return Step::Finish;
            };
            self.chart.update(cx, |chart, _| {
                if chart
                    .try_queue_replay_update(ReplayStreamUpdate::Snapshot(snapshot))
                    .is_err()
                {
                    eprintln!("windowed benchmark timeframe snapshot queue overflowed");
                }
            });
            window.refresh();
            self.timeframe_switch_submitted_at = Some(Instant::now());
            return Step::Continue;
        }
        let publication = match self.worker.publish_delta(self.previous_sequence) {
            Ok(Some(publication)) => publication,
            Ok(None) => return Step::Finish,
            Err(error) => {
                eprintln!("windowed benchmark replay failed: {error}");
                return Step::Finish;
            }
        };
        self.previous_sequence = publication.generation.sequence_range().1;
        let update = publication.update;
        self.chart.update(cx, |chart, _| {
            if chart.try_queue_replay_update(update).is_err() {
                eprintln!("windowed benchmark chart queue overflowed");
            }
        });
        window.refresh();
        self.iteration += 1;
        self.submitted_at = Instant::now();
        Step::Continue
    }

    #[cfg(target_os = "windows")]
    fn sample_composition(&mut self) {
        if let Some(sample) = self
            .composition_probe
            .as_ref()
            .and_then(|probe| probe.sample().ok())
        {
            self.composition_samples.push(sample);
        }
    }

    #[cfg(target_os = "windows")]
    fn finish_composition(&mut self) {
        self.final_composition_flush_succeeded = self
            .composition_probe
            .as_ref()
            .is_some_and(|probe| probe.flush().is_ok());
        self.sample_composition();
    }
}

struct WindowedBenchmarkApp {
    chart: Entity<OriginChartView>,
}

impl Render for WindowedBenchmarkApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.chart.clone())
    }
}

fn schedule_frame(driver: Rc<RefCell<BenchmarkDriver>>, window: &mut Window, _cx: &mut App) {
    window.on_next_frame(move |window, cx| {
        let finished = {
            let mut borrowed = driver.borrow_mut();
            matches!(borrowed.step(window, cx), Step::Finish)
        };
        if finished {
            #[cfg(target_os = "windows")]
            driver.borrow_mut().finish_composition();
            if let Err(error) = write_report(&driver.borrow()) {
                eprintln!("windowed benchmark report failed: {error}");
            }
            cx.quit();
            return;
        }
        schedule_frame(Rc::clone(&driver), window, cx);
    });
}

fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * numerator).div_ceil(denominator);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn latency_evidence(samples: &[FrameSample], select: fn(&FrameSample) -> u64) -> LatencyEvidence {
    let mut values: Vec<u64> = samples.iter().map(select).collect();
    values.sort_unstable();
    LatencyEvidence {
        p50: percentile(&values, 50, 100),
        p95: percentile(&values, 95, 100),
        p99: percentile(&values, 99, 100),
        p99_9: percentile(&values, 999, 1_000),
        maximum: values.last().copied().unwrap_or(0),
    }
}

#[cfg(target_os = "windows")]
fn windows_composition_evidence(driver: &BenchmarkDriver) -> WindowsCompositionEvidence {
    let samples = &driver.composition_samples;
    let first = samples.first().copied();
    let last = samples.last().copied();
    let (displayed_intervals, refresh_intervals) = composition_intervals(samples);
    let displayed_frame_advances = counter_delta(
        first.map(WindowsCompositionTiming::displayed_frame),
        last.map(WindowsCompositionTiming::displayed_frame),
    );
    let timeline_advanced = counter_delta(
        first.map(WindowsCompositionTiming::refresh_count),
        last.map(WindowsCompositionTiming::refresh_count),
    ) > 0;
    WindowsCompositionEvidence {
        availability: if samples.is_empty() {
            "unavailable"
        } else {
            "available"
        },
        timeline_observation: if timeline_advanced {
            "refresh_counter_advanced"
        } else {
            "no_refresh_counter_advance_observed"
        },
        scope: "windows_dwm_primary_output_compositor_timeline",
        semantics: "dwm_refresh_displayed_and_completed_counters_are_compositor_evidence_not_physical_scanout",
        qpc_frequency_hz: first.map(WindowsCompositionTiming::qpc_frequency_hz),
        reported_refresh_period_nanos: first
            .and_then(WindowsCompositionTiming::refresh_period_nanos),
        samples: samples.len(),
        initial_flush: flush_status(driver.initial_composition_flush_succeeded),
        final_flush: flush_status(driver.final_composition_flush_succeeded),
        first_vblank_qpc: first.map(WindowsCompositionTiming::vblank_qpc),
        last_vblank_qpc: last.map(WindowsCompositionTiming::vblank_qpc),
        first_compose_qpc: first.map(WindowsCompositionTiming::compose_qpc),
        last_compose_qpc: last.map(WindowsCompositionTiming::compose_qpc),
        first_displayed_frame: first.map(WindowsCompositionTiming::displayed_frame),
        last_displayed_frame: last.map(WindowsCompositionTiming::displayed_frame),
        displayed_frame_advances,
        displayed_frame_interval: interval_evidence(&displayed_intervals),
        first_completed_frame: first.map(WindowsCompositionTiming::completed_frame),
        last_completed_frame: last.map(WindowsCompositionTiming::completed_frame),
        completed_frame_advances: counter_delta(
            first.map(WindowsCompositionTiming::completed_frame),
            last.map(WindowsCompositionTiming::completed_frame),
        ),
        refresh_advances: counter_delta(
            first.map(WindowsCompositionTiming::refresh_count),
            last.map(WindowsCompositionTiming::refresh_count),
        ),
        refresh_interval: interval_evidence(&refresh_intervals),
        late_frame_delta: counter_delta(
            first.map(WindowsCompositionTiming::frames_late),
            last.map(WindowsCompositionTiming::frames_late),
        ),
        dropped_frame_delta: counter_delta(
            first.map(WindowsCompositionTiming::frames_dropped),
            last.map(WindowsCompositionTiming::frames_dropped),
        ),
        missed_frame_delta: counter_delta(
            first.map(WindowsCompositionTiming::frames_missed),
            last.map(WindowsCompositionTiming::frames_missed),
        ),
    }
}

#[cfg(target_os = "windows")]
fn composition_intervals(
    samples: &[WindowsCompositionTiming],
) -> (Vec<FrameSample>, Vec<FrameSample>) {
    let mut displayed_intervals = Vec::new();
    let mut refresh_intervals = Vec::new();
    for pair in samples.windows(2) {
        let [previous, current] = pair else {
            continue;
        };
        if let Some(nanos) = displayed_frame_interval_nanos(
            previous.displayed_frame(),
            previous.displayed_qpc(),
            current.displayed_frame(),
            current.displayed_qpc(),
            current.qpc_frequency_hz(),
        ) {
            displayed_intervals.push(FrameSample {
                update_to_frame_callback_nanos: nanos,
                frame_callback_interval_nanos: nanos,
            });
        }
        if let Some(nanos) = counter_interval_nanos(
            previous.refresh_count(),
            previous.vblank_qpc(),
            current.refresh_count(),
            current.vblank_qpc(),
            current.qpc_frequency_hz(),
        ) {
            refresh_intervals.push(FrameSample {
                update_to_frame_callback_nanos: nanos,
                frame_callback_interval_nanos: nanos,
            });
        }
    }
    (displayed_intervals, refresh_intervals)
}

#[cfg(target_os = "windows")]
fn interval_evidence(samples: &[FrameSample]) -> Option<LatencyEvidence> {
    (!samples.is_empty())
        .then(|| latency_evidence(samples, |sample| sample.frame_callback_interval_nanos))
}

#[cfg(target_os = "windows")]
const fn flush_status(succeeded: bool) -> &'static str {
    if succeeded {
        "succeeded"
    } else {
        "failed_or_unavailable"
    }
}

#[cfg(target_os = "windows")]
fn counter_delta(first: Option<u64>, last: Option<u64>) -> u64 {
    last.zip(first)
        .map_or(0, |(last, first)| last.saturating_sub(first))
}

#[cfg(target_os = "windows")]
fn displayed_frame_interval_nanos(
    previous_frame: u64,
    previous_qpc: u64,
    current_frame: u64,
    current_qpc: u64,
    qpc_frequency_hz: u64,
) -> Option<u64> {
    counter_interval_nanos(
        previous_frame,
        previous_qpc,
        current_frame,
        current_qpc,
        qpc_frequency_hz,
    )
}

#[cfg(target_os = "windows")]
fn counter_interval_nanos(
    previous_counter: u64,
    previous_qpc: u64,
    current_counter: u64,
    current_qpc: u64,
    qpc_frequency_hz: u64,
) -> Option<u64> {
    let counter_advance = current_counter.checked_sub(previous_counter)?;
    let tick_advance = current_qpc.checked_sub(previous_qpc)?;
    if counter_advance == 0 || tick_advance == 0 || qpc_frequency_hz == 0 {
        return None;
    }
    u64::try_from(
        u128::from(tick_advance) * 1_000_000_000
            / u128::from(qpc_frequency_hz)
            / u128::from(counter_advance),
    )
    .ok()
}

fn display_evidence() -> DisplayEvidence {
    match NativeDisplayProbe::probe() {
        Ok(environment) => DisplayEvidence {
            outputs: environment
                .outputs()
                .iter()
                .map(|output: &DisplayOutput| DisplayOutputEvidence {
                    name: output.name().map(str::to_string),
                    description: output.description().map(str::to_string),
                    refresh_millihertz: output.refresh_millihertz(),
                    refresh_interval_nanos: output.refresh_interval_nanos(),
                    pixel_size: output.pixel_size(),
                    effective_scale_milli: output.effective_scale_milli(),
                })
                .collect(),
            presentation_clock: environment
                .presentation_clock()
                .map(|clock| format!("{clock:?}")),
            probe: native_probe_name(),
        },
        Err(_) => DisplayEvidence {
            outputs: Vec::new(),
            presentation_clock: None,
            probe: "unavailable",
        },
    }
}

const fn native_probe_name() -> &'static str {
    #[cfg(target_os = "linux")]
    return "wayland";

    #[cfg(target_os = "windows")]
    return "windows_display_configuration";

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    "unavailable"
}

fn write_report(driver: &BenchmarkDriver) -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "windows")]
    let windows_dwm_composition = windows_composition_evidence(driver);
    let report = WindowedBenchmarkReport {
        schema_version: 5,
        evidence_scope: "windowed_first_pixel_timeframe_switch_replay_and_native_compositor_timeline",
        source_revision: env::var("GITHUB_SHA")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        display: display_evidence(),
        warmup_frames: WARMUP_FRAMES,
        measured_frames: driver.samples.len(),
        updates_published: driver.iteration,
        first_pixel_nanos: driver.first_pixel_nanos.unwrap_or(u64::MAX),
        timeframe_switch_first_pixel_nanos: driver
            .timeframe_switch_first_pixel_nanos
            .unwrap_or(u64::MAX),
        latency_targets: LatencyTargets {
            warm_first_pixel_met: driver
                .first_pixel_nanos
                .is_some_and(|value| value < 1_000_000_000),
            timeframe_switch_met: driver
                .timeframe_switch_first_pixel_nanos
                .is_some_and(|value| value < 1_000_000_000),
        },
        update_to_frame_callback: latency_evidence(&driver.samples, |sample| {
            sample.update_to_frame_callback_nanos
        }),
        frame_callback_interval: latency_evidence(&driver.samples, |sample| {
            sample.frame_callback_interval_nanos
        }),
        renderer_submission_performed: true,
        #[cfg(target_os = "windows")]
        compositor_presentation_evidence: if windows_dwm_composition.timeline_observation
            == "refresh_counter_advanced"
        {
            "windows_dwm_timeline_advanced"
        } else {
            "unavailable_or_static"
        },
        #[cfg(not(target_os = "windows"))]
        compositor_presentation_evidence: "unavailable",
        #[cfg(target_os = "windows")]
        windows_dwm_composition,
        physical_presentation_measured: false,
        presentation_measurement_method: presentation_measurement_method(),
        external_scanout_instrumented: false,
        limitations: [
            "gpui_frame_callback_cadence_not_physical_scanout",
            "dwm_displayed_counter_not_physical_panel_scanout",
            "single_named_display_profile",
            "disconnected_fixture_data_source",
            "window_output_attribution_by_compositor_placement",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(&driver.report_path, encoded)?;
    println!(
        "windowed_chart_latency=completed first_pixel_ns={} timeframe_switch_first_pixel_ns={} measured_frames={} update_to_frame_callback_p50_ns={} report={}",
        report.first_pixel_nanos,
        report.timeframe_switch_first_pixel_nanos,
        driver.samples.len(),
        report.update_to_frame_callback.p50,
        driver.report_path.display()
    );
    Ok(())
}

const fn presentation_measurement_method() -> &'static str {
    #[cfg(target_os = "windows")]
    return "gpui_on_next_frame_plus_dwm_composition_timing_and_flush";

    #[cfg(not(target_os = "windows"))]
    "gpui_on_next_frame_after_prior_render"
}

/// Runs the windowed benchmark and exits the process when measurement completes.
pub(crate) fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut worker = FixtureMarketWorker::try_new().map_err(Box::<dyn Error>::from)?;
    let bootstrap = worker
        .publish_snapshot(SNAPSHOT_BARS)
        .map_err(Box::<dyn Error>::from)?;
    let source_bars = bootstrap
        .snapshot
        .stream()
        .items()
        .iter()
        .map(|item| *item.value())
        .collect::<Vec<_>>();
    let (timeframe_bars, _) = aggregate_coinbase_bars(&source_bars, CoinbaseInterval::Minute5)
        .map_err(Box::<dyn Error>::from)?;
    let timeframe_snapshot = ReplaySnapshot::try_new(
        bootstrap.snapshot.instrument().clone(),
        ReplayProvenance::EmbeddedFixture,
        BarDefinition {
            definition_id: "coinbase_5m_ohlcv_v1".to_string(),
            version: 1,
            interval_seconds: 300,
            trades_per_bar: None,
        },
        timeframe_bars,
    )?
    .try_with_publication_generation(
        bootstrap
            .snapshot
            .evidence()
            .publication_generation
            .saturating_add(1),
    )?;
    let previous_sequence = bootstrap.snapshot.stream().last_sequence();
    let snapshot = bootstrap.snapshot;
    let report_path = report_path.to_path_buf();

    application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1_280.0), px(820.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let chart = cx.new(move |_| OriginChartView::with_replay(&snapshot));
                #[cfg(target_os = "windows")]
                let (composition_probe, initial_composition_flush_succeeded, composition_samples) =
                    match WindowsCompositionProbe::new() {
                        Ok(probe) => {
                            let flushed = probe.flush().is_ok();
                            let samples = probe.sample().into_iter().collect();
                            (Some(probe), flushed, samples)
                        }
                        Err(_) => (None, false, Vec::new()),
                    };
                let driver = Rc::new(RefCell::new(BenchmarkDriver {
                    chart: chart.clone(),
                    worker,
                    timeframe_snapshot: Some(timeframe_snapshot),
                    previous_sequence,
                    iteration: 0,
                    submitted_at: Instant::now(),
                    last_callback_at: Instant::now(),
                    samples: Vec::with_capacity(MEASURED_FRAMES),
                    report_path,
                    opened_at: Instant::now(),
                    first_pixel_nanos: None,
                    timeframe_switch_submitted_at: None,
                    timeframe_switch_first_pixel_nanos: None,
                    #[cfg(target_os = "windows")]
                    composition_probe,
                    #[cfg(target_os = "windows")]
                    composition_samples,
                    #[cfg(target_os = "windows")]
                    initial_composition_flush_succeeded,
                    #[cfg(target_os = "windows")]
                    final_composition_flush_succeeded: false,
                }));
                schedule_frame(driver, window, cx);
                cx.new(|cx| {
                    gpui_component::Root::new(
                        cx.new(|_| WindowedBenchmarkApp { chart }),
                        window,
                        cx,
                    )
                })
            },
        )
        .expect("the windowed benchmark window opens");
        cx.activate(true);
    });
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::displayed_frame_interval_nanos;

    #[test]
    fn displayed_interval_normalizes_multi_frame_counter_advances() {
        assert_eq!(
            displayed_frame_interval_nanos(100, 10_000, 103, 10_600, 10_000),
            Some(20_000_000)
        );
        assert_eq!(
            displayed_frame_interval_nanos(100, 10_000, 100, 10_600, 10_000),
            None
        );
    }
}
