//! Windowed replay-to-frame-callback benchmark (`S1-13` and Stage E evidence path).
//!
//! Drives deterministic replay deltas through the real GPUI window one update per
//! frame and measures update-to-next-frame latency, covering-snapshot installation,
//! frame registration, production input/instrument/interval handler duration,
//! callback cadence, process working-set growth, and chart-queue occupancy from
//! GPUI's `on_next_frame` callbacks against the display profile reported by
//! [`NativeDisplayProbe`].
//! Renderer submission is genuinely performed through a native GPUI window. These
//! callbacks run after the prior render but do not prove physical scanout, and the
//! report says so.

use axiusflow_application::{EmbeddedReplaySource, LoadEmbeddedReplay, ReplaySnapshot};
use axiusflow_chart_integration::{ChartBridgeMetrics, NucleusChartView};
use axiusflow_desktop::market_worker::{
    CoinbaseWorkerStartup, FixtureMarketWorker, MarketDataWorker, MarketWorkerCommand,
    MarketWorkerSender, MarketWorkerStartup, market_worker_channel,
};
use axiusflow_engine_protocol::InstallProviderInstrument;
use axiusflow_market_data::ChartInterval;
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
    num::NonZeroUsize,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::AtomicU64,
        mpsc::{Receiver, TryRecvError, channel, sync_channel},
    },
    time::Instant,
};

use crate::native_ui::input::{InputEvent, InputState};
use crate::readiness_conformance::ProcessMemoryProbe;
use crate::{
    DesktopLifecycle, DesktopLifetimeMode, WorkspaceSurface, chart_pane_host,
    subscribe_symbol_input,
};

const SNAPSHOT_BARS: usize = 256;
const REPLACEMENT_SNAPSHOT_BARS: usize = 600;
const WARMUP_FRAMES: usize = 32;
const MEASURED_FRAMES: usize = 256;
const INTERACTION_SAMPLES: usize = 128;
const INTERACTION_COMMAND_CAPACITY: usize = 4;

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
    snapshot_replacement_met: bool,
}

#[derive(Serialize)]
struct ProcessMemoryBytes {
    baseline: u64,
    current: u64,
    observed_high_water: u64,
    observed_growth: u64,
}

#[derive(Serialize)]
struct ChartQueueEvidence {
    maximum_pending_before_submission: usize,
    maximum_pending_after_submission: usize,
    overflows: u64,
    drained_between_frames: bool,
    one_update_per_frame: bool,
    no_overflow: bool,
}

#[derive(Serialize)]
struct ForegroundDurationEvidence {
    chart_snapshot_installation_samples: usize,
    chart_snapshot_installation: LatencyEvidence,
    frame_scheduling_samples: usize,
    frame_scheduling: LatencyEvidence,
    symbol_input_change_samples: usize,
    symbol_input_change: LatencyEvidence,
    symbol_input_submit_samples: usize,
    symbol_input_submit: LatencyEvidence,
    instrument_selection_samples: usize,
    instrument_selection: LatencyEvidence,
    interval_selection_samples: usize,
    interval_selection: LatencyEvidence,
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
    snapshot_replacement_first_pixel_nanos: u64,
    latency_targets: LatencyTargets,
    update_to_frame_callback: LatencyEvidence,
    frame_callback_interval: LatencyEvidence,
    process_memory_bytes: ProcessMemoryBytes,
    chart_queue: ChartQueueEvidence,
    foreground_duration: ForegroundDurationEvidence,
    renderer_submission_performed: bool,
    compositor_presentation_evidence: &'static str,
    #[cfg(target_os = "windows")]
    windows_dwm_composition: WindowsCompositionEvidence,
    physical_presentation_measured: bool,
    presentation_measurement_method: &'static str,
    external_scanout_instrumented: bool,
    limitations: [&'static str; 7],
}

struct FrameSample {
    update_to_frame_callback_nanos: u64,
    frame_callback_interval_nanos: u64,
}

struct BenchmarkDriver {
    chart: Entity<NucleusChartView>,
    terminal: Entity<WorkspaceSurface>,
    symbol_input: Entity<InputState>,
    worker: FixtureMarketWorker,
    _message_sender: MarketWorkerSender,
    interaction_commands: Receiver<MarketWorkerCommand>,
    replacement_snapshot: Option<ReplaySnapshot>,
    previous_sequence: u64,
    iteration: usize,
    submitted_at: Instant,
    last_callback_at: Instant,
    samples: Vec<FrameSample>,
    report_path: PathBuf,
    opened_at: Instant,
    first_pixel_nanos: Option<u64>,
    snapshot_replacement_submitted_at: Option<Instant>,
    snapshot_replacement_first_pixel_nanos: Option<u64>,
    memory: ProcessMemoryProbe,
    maximum_pending_before_submission: usize,
    maximum_pending_after_submission: usize,
    chart_queue_overflows: u64,
    chart_snapshot_installation_nanos: Vec<u64>,
    frame_scheduling_nanos: Vec<u64>,
    interaction_iterations: usize,
    failure: Option<String>,
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

type BenchmarkOutcome = Rc<RefCell<Option<Result<(), String>>>>;

struct BenchmarkSetup {
    memory: ProcessMemoryProbe,
    worker: FixtureMarketWorker,
    replacement_snapshot: ReplaySnapshot,
    previous_sequence: u64,
    snapshot: ReplaySnapshot,
    report_path: PathBuf,
    interaction_worker: MarketDataWorker,
    interaction_commands: Receiver<MarketWorkerCommand>,
    message_sender: MarketWorkerSender,
    coinbase_products: [InstallProviderInstrument; 2],
    interaction_startup: MarketWorkerStartup,
}

impl BenchmarkDriver {
    fn step(&mut self, window: &mut Window, cx: &mut App) -> Step {
        let callback_at = Instant::now();
        let pending_before_submission = self.observe_chart_before_submission(cx);
        self.maximum_pending_before_submission = self
            .maximum_pending_before_submission
            .max(pending_before_submission.queued_updates);
        self.chart_queue_overflows = self
            .chart_queue_overflows
            .max(pending_before_submission.queue_overflows);
        self.first_pixel_nanos.get_or_insert_with(|| {
            u64::try_from(
                callback_at
                    .saturating_duration_since(self.opened_at)
                    .as_nanos(),
            )
            .unwrap_or(u64::MAX)
        });
        if let Some(submitted_at) = self.snapshot_replacement_submitted_at.take() {
            self.snapshot_replacement_first_pixel_nanos = Some(
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
            return self.step_after_replay(window, cx);
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
        let metrics = self.chart.update(cx, |chart, _| {
            if chart.try_queue_replay_update(update).is_err() {
                eprintln!("windowed benchmark chart queue overflowed");
            }
            chart.replay_bridge_metrics()
        });
        self.maximum_pending_after_submission = self
            .maximum_pending_after_submission
            .max(metrics.queued_updates);
        self.chart_queue_overflows = self.chart_queue_overflows.max(metrics.queue_overflows);
        window.refresh();
        self.iteration += 1;
        self.submitted_at = Instant::now();
        Step::Continue
    }

    fn step_after_replay(&mut self, window: &mut Window, cx: &mut App) -> Step {
        if self.snapshot_replacement_first_pixel_nanos.is_some() {
            if self.interaction_iterations >= INTERACTION_SAMPLES {
                if let Err(error) = self.drain_interaction_commands() {
                    self.failure = Some(error);
                }
                return Step::Finish;
            }
            if let Err(error) = self.sample_interactions(cx) {
                self.failure = Some(error);
                return Step::Finish;
            }
            window.refresh();
            return Step::Continue;
        }
        let Some(snapshot) = self.replacement_snapshot.take() else {
            return Step::Finish;
        };
        let submitted_at = Instant::now();
        if let Err(error) = self
            .chart
            .update(cx, |chart, _| chart.load_replay(&snapshot))
        {
            self.failure = Some(format!(
                "windowed benchmark covering snapshot replacement failed: {error}"
            ));
            return Step::Finish;
        }
        window.refresh();
        self.snapshot_replacement_submitted_at = Some(submitted_at);
        Step::Continue
    }

    fn sample_interactions(&mut self, cx: &mut App) -> Result<(), String> {
        if self.interaction_iterations > 0 {
            self.drain_interaction_commands()?;
        }
        let instrument_index = usize::from(self.interaction_iterations.is_multiple_of(2));
        self.terminal.update(cx, |terminal, _| {
            terminal.chrome_selection = instrument_index;
        });
        self.symbol_input.update(cx, |_, input_cx| {
            input_cx.emit(InputEvent::PressEnter {
                secondary: false,
                shift: false,
            });
        });
        self.symbol_input
            .update(cx, |_, input_cx| input_cx.emit(InputEvent::Change));
        let interval = if self.interaction_iterations.is_multiple_of(2) {
            ChartInterval::Minute5
        } else {
            ChartInterval::Minute15
        };
        let selected = self.terminal.update(cx, |terminal, terminal_cx| {
            terminal.select_interval(interval, terminal_cx)
        });
        if !selected {
            return Err("windowed benchmark timeframe handler rejected fixture selection".into());
        }
        self.interaction_iterations += 1;
        Ok(())
    }

    fn drain_interaction_commands(&self) -> Result<(), String> {
        let mut commands = 0;
        loop {
            match self.interaction_commands.try_recv() {
                Ok(MarketWorkerCommand::CoinbaseSelect(_)) => commands += 1,
                Ok(_) => {
                    return Err(
                        "windowed benchmark interaction emitted an unexpected worker command"
                            .into(),
                    );
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err("windowed benchmark interaction command sink disconnected".into());
                }
            }
        }
        if commands != 2 {
            return Err(format!(
                "windowed benchmark expected symbol and timeframe commands, observed {commands}"
            ));
        }
        Ok(())
    }

    fn observe_chart_before_submission(&mut self, cx: &mut App) -> ChartBridgeMetrics {
        let (metrics, snapshot_installation_nanos) = self.chart.update(cx, |chart, _| {
            (
                chart.replay_bridge_metrics(),
                chart.take_snapshot_installation_nanos(),
            )
        });
        if let Some(elapsed_nanos) = snapshot_installation_nanos {
            self.chart_snapshot_installation_nanos.push(elapsed_nanos);
        }
        metrics
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
    chart: Entity<NucleusChartView>,
}

impl Render for WindowedBenchmarkApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .overflow_hidden()
            .child(chart_pane_host(Some(&self.chart)))
    }
}

fn schedule_frame(
    driver: Rc<RefCell<BenchmarkDriver>>,
    outcome: BenchmarkOutcome,
    window: &mut Window,
    _cx: &mut App,
) {
    let scheduling_started = Instant::now();
    let scheduling_driver = Rc::clone(&driver);
    window.on_next_frame(move |window, cx| {
        let finished = {
            let mut borrowed = driver.borrow_mut();
            matches!(borrowed.step(window, cx), Step::Finish)
        };
        if finished {
            #[cfg(target_os = "windows")]
            driver.borrow_mut().finish_composition();
            let result =
                write_report(&mut driver.borrow_mut(), cx).map_err(|error| error.to_string());
            if let Err(error) = &result {
                eprintln!("windowed benchmark report failed: {error}");
            }
            *outcome.borrow_mut() = Some(result);
            cx.quit();
            return;
        }
        schedule_frame(Rc::clone(&driver), Rc::clone(&outcome), window, cx);
    });
    scheduling_driver
        .borrow_mut()
        .frame_scheduling_nanos
        .push(elapsed_nanos(scheduling_started));
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
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

fn duration_latency_evidence(samples: &[u64]) -> LatencyEvidence {
    let mut values = samples.to_vec();
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

fn chart_queue_evidence(driver: &BenchmarkDriver) -> Result<ChartQueueEvidence, String> {
    let drained_between_frames = driver.maximum_pending_before_submission == 0;
    let one_update_per_frame = driver.maximum_pending_after_submission <= 1;
    let no_overflow = driver.chart_queue_overflows == 0;
    if !drained_between_frames || !one_update_per_frame || !no_overflow {
        return Err(format!(
            "windowed benchmark chart-queue bound failed: before={} after={} overflows={}",
            driver.maximum_pending_before_submission,
            driver.maximum_pending_after_submission,
            driver.chart_queue_overflows
        ));
    }
    Ok(ChartQueueEvidence {
        maximum_pending_before_submission: driver.maximum_pending_before_submission,
        maximum_pending_after_submission: driver.maximum_pending_after_submission,
        overflows: driver.chart_queue_overflows,
        drained_between_frames,
        one_update_per_frame,
        no_overflow,
    })
}

fn foreground_duration_evidence(
    driver: &BenchmarkDriver,
    cx: &App,
) -> Result<ForegroundDurationEvidence, String> {
    let interactions = &driver.terminal.read(cx).foreground_interactions;
    let interaction_counts = [
        interactions.symbol_input_change.len(),
        interactions.symbol_input_submit.len(),
        interactions.instrument_selection.len(),
        interactions.interval_selection.len(),
    ];
    if driver.chart_snapshot_installation_nanos.is_empty()
        || driver.frame_scheduling_nanos.is_empty()
        || interaction_counts
            .iter()
            .any(|samples| *samples != INTERACTION_SAMPLES)
    {
        return Err(format!(
            "windowed benchmark foreground timing missing: snapshot_installations={} frame_schedules={} input_change={} input_submit={} instrument={} interval={}",
            driver.chart_snapshot_installation_nanos.len(),
            driver.frame_scheduling_nanos.len(),
            interaction_counts[0],
            interaction_counts[1],
            interaction_counts[2],
            interaction_counts[3],
        ));
    }
    Ok(ForegroundDurationEvidence {
        chart_snapshot_installation_samples: driver.chart_snapshot_installation_nanos.len(),
        chart_snapshot_installation: duration_latency_evidence(
            &driver.chart_snapshot_installation_nanos,
        ),
        frame_scheduling_samples: driver.frame_scheduling_nanos.len(),
        frame_scheduling: duration_latency_evidence(&driver.frame_scheduling_nanos),
        symbol_input_change_samples: interaction_counts[0],
        symbol_input_change: duration_latency_evidence(&interactions.symbol_input_change),
        symbol_input_submit_samples: interaction_counts[1],
        symbol_input_submit: duration_latency_evidence(&interactions.symbol_input_submit),
        instrument_selection_samples: interaction_counts[2],
        instrument_selection: duration_latency_evidence(&interactions.instrument_selection),
        interval_selection_samples: interaction_counts[3],
        interval_selection: duration_latency_evidence(&interactions.interval_selection),
    })
}

fn verify_chart_viewport(driver: &BenchmarkDriver, cx: &App) -> Result<(), String> {
    let (width, height) = driver.chart.read(cx).rendered_viewport_size();
    if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
        Ok(())
    } else {
        Err(format!(
            "windowed benchmark chart viewport is not drawable: {width}x{height}"
        ))
    }
}

fn write_report(driver: &mut BenchmarkDriver, cx: &App) -> Result<(), Box<dyn Error>> {
    if let Some(failure) = driver.failure.take() {
        return Err(failure.into());
    }
    verify_chart_viewport(driver, cx)?;
    driver.memory.sample()?;
    let observed_growth_bytes = driver.memory.observed_growth_bytes();
    let chart_queue = chart_queue_evidence(driver)?;
    let foreground_duration = foreground_duration_evidence(driver, cx)?;
    #[cfg(target_os = "windows")]
    let windows_dwm_composition = windows_composition_evidence(driver);
    let report = WindowedBenchmarkReport {
        schema_version: 8,
        evidence_scope: "windowed_first_pixel_snapshot_replacement_replay_memory_queue_production_interaction_handlers_and_native_compositor_timeline",
        source_revision: env::var("GITHUB_SHA")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        display: display_evidence(),
        warmup_frames: WARMUP_FRAMES,
        measured_frames: driver.samples.len(),
        updates_published: driver.iteration,
        first_pixel_nanos: driver.first_pixel_nanos.unwrap_or(u64::MAX),
        snapshot_replacement_first_pixel_nanos: driver
            .snapshot_replacement_first_pixel_nanos
            .unwrap_or(u64::MAX),
        latency_targets: LatencyTargets {
            warm_first_pixel_met: driver
                .first_pixel_nanos
                .is_some_and(|value| value < 1_000_000_000),
            snapshot_replacement_met: driver
                .snapshot_replacement_first_pixel_nanos
                .is_some_and(|value| value < 1_000_000_000),
        },
        update_to_frame_callback: latency_evidence(&driver.samples, |sample| {
            sample.update_to_frame_callback_nanos
        }),
        frame_callback_interval: latency_evidence(&driver.samples, |sample| {
            sample.frame_callback_interval_nanos
        }),
        process_memory_bytes: ProcessMemoryBytes {
            baseline: driver.memory.baseline_bytes(),
            current: driver.memory.current_bytes(),
            observed_high_water: driver.memory.high_water_bytes(),
            observed_growth: observed_growth_bytes,
        },
        chart_queue,
        foreground_duration,
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
            "interaction_handlers_use_a_bounded_disconnected_command_sink",
            "tab_switch_unmeasured_because_the_current_terminal_has_no_tab_surface",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(&driver.report_path, encoded)?;
    println!(
        "windowed_chart_latency=completed first_pixel_ns={} snapshot_replacement_first_pixel_ns={} measured_frames={} update_to_frame_callback_p50_ns={} snapshot_installation_ns={} frame_schedule_p99_ns={} input_change_p99_ns={} input_submit_p99_ns={} instrument_selection_p99_ns={} interval_selection_p99_ns={} process_memory_growth_bytes={} chart_queue_before={} chart_queue_after={} chart_queue_overflows={} report={}",
        report.first_pixel_nanos,
        report.snapshot_replacement_first_pixel_nanos,
        driver.samples.len(),
        report.update_to_frame_callback.p50,
        report
            .foreground_duration
            .chart_snapshot_installation
            .maximum,
        report.foreground_duration.frame_scheduling.p99,
        report.foreground_duration.symbol_input_change.p99,
        report.foreground_duration.symbol_input_submit.p99,
        report.foreground_duration.instrument_selection.p99,
        report.foreground_duration.interval_selection.p99,
        report.process_memory_bytes.observed_growth,
        report.chart_queue.maximum_pending_before_submission,
        report.chart_queue.maximum_pending_after_submission,
        report.chart_queue.overflows,
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

fn clear_report_path(report_path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::remove_file(report_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn benchmark_coinbase_product(base: &str) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: format!("instrument:coinbase:{}:usd", base.to_ascii_lowercase()),
        provider_symbol: format!("{base}-USD"),
        display_symbol: format!("{base}/USD"),
        venue_id: "coinbase".to_string(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: "crypto_public_realtime".to_string(),
    }
}

fn benchmark_root(
    setup: BenchmarkSetup,
    outcome: BenchmarkOutcome,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WindowedBenchmarkApp> {
    let BenchmarkSetup {
        memory,
        worker,
        replacement_snapshot,
        previous_sequence,
        snapshot,
        report_path,
        interaction_worker,
        interaction_commands,
        message_sender,
        coinbase_products,
        interaction_startup,
    } = setup;
    let chart = cx.new(move |_| NucleusChartView::with_replay(&snapshot));
    let symbol_input = cx.new(|cx| InputState::new(window, cx));
    let indicator_input = cx.new(|cx| InputState::new(window, cx));
    let terminal_symbol_input = symbol_input.clone();
    let terminal = cx.new(move |cx| {
        WorkspaceSurface::new(
            cx,
            interaction_startup,
            interaction_worker,
            DesktopLifecycle::new(DesktopLifetimeMode::KeepEngineWarm, false, false)
                .expect("benchmark lifecycle client starts"),
            Some(terminal_symbol_input),
            indicator_input,
        )
    });
    terminal.update(cx, |terminal, _| {
        terminal.coinbase_products = coinbase_products.into();
    });
    subscribe_symbol_input(Some(symbol_input.clone()), &terminal, window, cx);
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
        terminal,
        symbol_input,
        worker,
        _message_sender: message_sender,
        interaction_commands,
        replacement_snapshot: Some(replacement_snapshot),
        previous_sequence,
        iteration: 0,
        submitted_at: Instant::now(),
        last_callback_at: Instant::now(),
        samples: Vec::with_capacity(MEASURED_FRAMES),
        report_path,
        opened_at: Instant::now(),
        first_pixel_nanos: None,
        snapshot_replacement_submitted_at: None,
        snapshot_replacement_first_pixel_nanos: None,
        memory,
        maximum_pending_before_submission: 0,
        maximum_pending_after_submission: 0,
        chart_queue_overflows: 0,
        chart_snapshot_installation_nanos: Vec::with_capacity(1),
        frame_scheduling_nanos: Vec::with_capacity(
            WARMUP_FRAMES + MEASURED_FRAMES + INTERACTION_SAMPLES + 3,
        ),
        interaction_iterations: 0,
        failure: None,
        #[cfg(target_os = "windows")]
        composition_probe,
        #[cfg(target_os = "windows")]
        composition_samples,
        #[cfg(target_os = "windows")]
        initial_composition_flush_succeeded,
        #[cfg(target_os = "windows")]
        final_composition_flush_succeeded: false,
    }));
    schedule_frame(driver, outcome, window, cx);
    cx.new(|_| WindowedBenchmarkApp { chart })
}

fn run_application(setup: BenchmarkSetup, outcome: BenchmarkOutcome) {
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1_280.0), px(820.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| benchmark_root(setup, outcome, window, cx),
        )
        .expect("the windowed benchmark window opens");
        cx.activate(true);
    });
}

/// Runs the windowed benchmark and exits the process when measurement completes.
pub(crate) fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    clear_report_path(report_path)?;
    let outcome = Rc::new(RefCell::new(None));
    let application_outcome = Rc::clone(&outcome);
    let memory = ProcessMemoryProbe::new()?;
    let mut worker = FixtureMarketWorker::try_new().map_err(std::io::Error::other)?;
    let bootstrap = worker
        .publish_snapshot(SNAPSHOT_BARS)
        .map_err(std::io::Error::other)?;
    let replacement_snapshot = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay {
            bar_count: REPLACEMENT_SNAPSHOT_BARS,
        })?
        .try_with_publication_generation(
            bootstrap
                .snapshot
                .evidence()
                .publication_generation
                .saturating_add(1),
        )?;
    let previous_sequence = bootstrap.snapshot.stream().last_sequence();
    let (snapshot, report_path) = (bootstrap.snapshot, report_path.to_path_buf());
    let (interaction_command_sender, interaction_commands) =
        sync_channel(INTERACTION_COMMAND_CAPACITY);
    let (message_sender, message_receiver) = market_worker_channel(NonZeroUsize::MIN);
    let (shutdown_sender, shutdown_receiver) = channel();
    shutdown_sender.send(())?;
    let interaction_worker = MarketDataWorker::from_channels(
        interaction_command_sender,
        message_receiver,
        shutdown_receiver,
        None,
        Some(Arc::new(AtomicU64::new(0))),
    );
    let coinbase_products = [
        benchmark_coinbase_product("BTC"),
        benchmark_coinbase_product("ETH"),
    ];
    let interaction_startup = MarketWorkerStartup::Loading(Box::new(CoinbaseWorkerStartup {
        coinbase_product: coinbase_products[0].clone(),
        coinbase_interval: ChartInterval::Minute1,
        restored_viewport: None,
        subscription_id: "benchmark-interaction".to_string(),
        worker_label: "bounded disconnected benchmark sink".to_string(),
    }));
    run_application(
        BenchmarkSetup {
            memory,
            worker,
            replacement_snapshot,
            previous_sequence,
            snapshot,
            report_path,
            interaction_worker,
            interaction_commands,
            message_sender,
            coinbase_products,
            interaction_startup,
        },
        application_outcome,
    );
    let result = outcome
        .borrow_mut()
        .take()
        .ok_or("windowed benchmark exited before report completion")?;
    result.map_err(Into::into)
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
