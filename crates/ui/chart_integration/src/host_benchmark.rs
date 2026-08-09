//! Deterministic replay-to-Origin-to-GPUI host preparation sampling.

use crate::bridge::{ChartDataBridge, MergedChartData};
use crate::origin_bridge::{apply_merged_chart_data, install_replay, replay_price_divisor};
use axiusflow_application::{ReplaySnapshot, ReplayStreamUpdate, ReplayValidationError};
use core::fmt;
use origin_engine::{ChartEngine, ChartFrame, SeriesKind};
use origin_render_gpui::{GpuiChartRenderer, GpuiRenderError, PreparedOriginFrame};
use std::{error::Error, num::NonZeroUsize, time::Instant};

/// One deterministic replay-to-Origin-to-GPUI-host timing sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OriginGpuiHostSample {
    pub origin_frame_construction_nanos: u64,
    pub gpui_host_preparation_nanos: u64,
    pub origin_primitive_count: usize,
    pub origin_last_source_sequence: u64,
    pub gpui_plan_operations: u32,
    pub gpui_mesh_vertices: u32,
    pub submission_boundary_ready: bool,
    pub renderer_submission_performed: bool,
    pub physical_presentation_measured: bool,
}

/// Failures from the deterministic headless Origin/GPUI host boundary.
#[derive(Debug)]
pub enum OriginGpuiBenchmarkError {
    Replay(ReplayValidationError),
    QueueRejected,
    MissingMutation,
    Gpui(GpuiRenderError),
}

impl fmt::Display for OriginGpuiBenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Origin GPUI host benchmark failed: {self:?}")
    }
}

impl Error for OriginGpuiBenchmarkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Replay(error) => Some(error),
            Self::Gpui(error) => Some(error),
            Self::QueueRejected | Self::MissingMutation => None,
        }
    }
}

/// Applies one decoded incremental update through Origin and prepares the real GPUI scene plan.
///
/// The client snapshot and update remain canonical fixed-point values until
/// `apply_merged_chart_data` converts them at the Origin boundary. Origin owns all chart state.
/// This headless path reaches the renderer-submission preparation boundary but deliberately does
/// not claim a GPUI window submission or a physically presented pixel.
///
/// # Errors
///
/// Returns replay validation, bounded queue, missing-mutation, or GPUI planning failures.
pub fn run_origin_gpui_host_sample(
    baseline: &ReplaySnapshot,
    update: ReplayStreamUpdate,
) -> Result<OriginGpuiHostSample, OriginGpuiBenchmarkError> {
    const WIDTH: f64 = 1_280.0;
    const HEIGHT: f64 = 720.0;
    const SCALE_FACTOR: f32 = 1.0;

    let mut engine = ChartEngine::new(WIDTH, HEIGHT, f64::from(SCALE_FACTOR));
    install_replay(&mut engine, baseline);
    engine.series[0].kind = SeriesKind::Candlestick;
    let mut price_divisor = replay_price_divisor(baseline);
    let mut bridge = ChartDataBridge::try_new(NonZeroUsize::MIN, baseline)
        .map_err(OriginGpuiBenchmarkError::Replay)?;
    bridge
        .try_push(update)
        .map_err(|_| OriginGpuiBenchmarkError::QueueRejected)?;

    let origin_started = Instant::now();
    let merged = bridge
        .drain_merged()
        .map_err(OriginGpuiBenchmarkError::Replay)?
        .filter(MergedChartData::mutates_series)
        .ok_or(OriginGpuiBenchmarkError::MissingMutation)?;
    let origin_last_source_sequence = merged.accepted_deltas().last().map_or_else(
        || {
            merged
                .snapshot()
                .map_or(baseline.evidence().last_sequence, |snapshot| {
                    snapshot.evidence().last_sequence
                })
        },
        |item| item.value().source_sequence,
    );
    apply_merged_chart_data(&mut engine, &mut price_divisor, &merged);
    engine.css_width = WIDTH;
    engine.css_height = HEIGHT;
    engine.dpr = f64::from(SCALE_FACTOR);
    let measure =
        |text: &str| u32::try_from(text.len()).map_or(f64::from(u32::MAX), f64::from) * 7.0;
    engine.recompute_layout_with_measure(true, measure);
    engine.fit_content();
    engine.recompute_layout_with_measure(true, measure);
    let layout = engine.options.get().layout;
    let max_label_width = (layout.font_size + 4.0) * 5.0 / 8.0
        * f64::from(engine.tick_mark_max_character_length.max(1));
    let axis_frame = engine.build_axis_frame(max_label_width, measure);
    let mut frame = ChartFrame::default();
    let mut axis_prims = Vec::new();
    engine.build_frame_into(&mut frame);
    engine.build_axis_primitives_into(&axis_frame, &mut axis_prims, |_| 0.0);
    let origin_frame_construction_nanos =
        u64::try_from(origin_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let origin_primitive_count = frame
        .panes
        .iter()
        .map(|pane| {
            pane.under
                .len()
                .saturating_add(pane.main.len())
                .saturating_add(pane.top_prims.len())
        })
        .sum::<usize>()
        .saturating_add(axis_prims.len());

    let host_started = Instant::now();
    let prepared = PreparedOriginFrame::from_engine(&frame, &engine).with_axis(&axis_prims, &[]);
    let mut renderer = GpuiChartRenderer::new();
    let metrics = renderer
        .plan_frame(&prepared, SCALE_FACTOR)
        .map_err(OriginGpuiBenchmarkError::Gpui)?;
    let gpui_host_preparation_nanos =
        u64::try_from(host_started.elapsed().as_nanos()).unwrap_or(u64::MAX);

    Ok(OriginGpuiHostSample {
        origin_frame_construction_nanos,
        gpui_host_preparation_nanos,
        origin_primitive_count,
        origin_last_source_sequence,
        gpui_plan_operations: metrics.ops,
        gpui_mesh_vertices: metrics.mesh_vertices,
        submission_boundary_ready: metrics.prims > 0 && metrics.paint_nanos == 0,
        renderer_submission_performed: false,
        physical_presentation_measured: false,
    })
}
