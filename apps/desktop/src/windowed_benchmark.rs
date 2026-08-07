//! Windowed replay-to-frame-callback benchmark (`S1-13` and Stage E evidence path).
//!
//! Drives deterministic replay deltas through the real GPUI window one update per
//! frame and measures update-to-next-frame latency and callback cadence from GPUI's
//! `on_next_frame` callbacks against the display profile reported by
//! [`NativeDisplayProbe`]. Renderer submission is genuinely performed through a
//! native GPUI window. These callbacks run after the prior render but do not prove
//! physical scanout, and the report says so.

use crate::market_worker::FixtureMarketWorker;
use axiusflow_chart_integration::OriginChartView;
use axiusflow_design_system::AxiusflowTheme;
use axiusflow_platform_runtime::{DisplayOutput, NativeDisplayProbe};
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

#[derive(Serialize)]
struct LatencyEvidence {
    p50: u64,
    p95: u64,
    p99: u64,
    p99_9: u64,
    maximum: u64,
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
    update_to_frame_callback: LatencyEvidence,
    frame_callback_interval: LatencyEvidence,
    renderer_submission_performed: bool,
    physical_presentation_measured: bool,
    presentation_measurement_method: &'static str,
    external_scanout_instrumented: bool,
    limitations: [&'static str; 4],
}

struct FrameSample {
    update_to_frame_callback_nanos: u64,
    frame_callback_interval_nanos: u64,
}

struct BenchmarkDriver {
    chart: Entity<OriginChartView>,
    worker: FixtureMarketWorker,
    previous_sequence: u64,
    iteration: usize,
    submitted_at: Instant,
    last_callback_at: Instant,
    samples: Vec<FrameSample>,
    report_path: PathBuf,
}

enum Step {
    Continue,
    Finish,
}

impl BenchmarkDriver {
    fn step(&mut self, window: &mut Window, cx: &mut App) -> Step {
        let callback_at = Instant::now();
        if self.iteration > 0 {
            let update_to_frame_callback = callback_at
                .saturating_duration_since(self.submitted_at)
                .as_nanos();
            let frame_callback_interval = callback_at
                .saturating_duration_since(self.last_callback_at)
                .as_nanos();
            if self.iteration > WARMUP_FRAMES {
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
            return Step::Finish;
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
    let report = WindowedBenchmarkReport {
        schema_version: 2,
        evidence_scope: "windowed_replay_to_frame_callback",
        source_revision: env::var("GITHUB_SHA")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        display: display_evidence(),
        warmup_frames: WARMUP_FRAMES,
        measured_frames: driver.samples.len(),
        updates_published: driver.iteration,
        update_to_frame_callback: latency_evidence(&driver.samples, |sample| {
            sample.update_to_frame_callback_nanos
        }),
        frame_callback_interval: latency_evidence(&driver.samples, |sample| {
            sample.frame_callback_interval_nanos
        }),
        renderer_submission_performed: true,
        physical_presentation_measured: false,
        presentation_measurement_method: "gpui_on_next_frame_after_prior_render",
        external_scanout_instrumented: false,
        limitations: [
            "frame_callback_cadence_not_physical_scanout",
            "single_named_display_profile",
            "disconnected_fixture_data_source",
            "window_output_attribution_by_compositor_placement",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(&driver.report_path, encoded)?;
    println!(
        "windowed_replay_to_frame_callback=completed measured_frames={} update_to_frame_callback_p50_ns={} frame_callback_interval_p50_ns={} report={}",
        driver.samples.len(),
        report.update_to_frame_callback.p50,
        report.frame_callback_interval.p50,
        driver.report_path.display()
    );
    Ok(())
}

/// Runs the windowed benchmark and exits the process when measurement completes.
pub(crate) fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut worker = FixtureMarketWorker::try_new().map_err(Box::<dyn Error>::from)?;
    let bootstrap = worker
        .publish_snapshot(SNAPSHOT_BARS)
        .map_err(Box::<dyn Error>::from)?;
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
                let theme = AxiusflowTheme::dark();
                let chart =
                    cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot));
                let driver = Rc::new(RefCell::new(BenchmarkDriver {
                    chart: chart.clone(),
                    worker,
                    previous_sequence,
                    iteration: 0,
                    submitted_at: Instant::now(),
                    last_callback_at: Instant::now(),
                    samples: Vec::with_capacity(MEASURED_FRAMES),
                    report_path,
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
