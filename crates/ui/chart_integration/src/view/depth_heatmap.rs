use super::{AerisChartView, ChartColor, DepthHeatmapSettings, OrderFlowSettings};
use aeris_charts_engine::{DepthHeatmapOptions, DepthLevel, DepthOptions, DepthSnapshot};
use num_traits::ToPrimitive;

/// One heatmap column per second of book history, kept for the most recent hour.
const HISTORY_BUCKET_MICROS: i64 = 1_000_000;
const MAX_HISTORY_BUCKETS: usize = 3_600;
/// Levels kept per side of each recorded book; deeper levels are dropped.
const MAX_LEVELS_PER_SIDE: usize = 200;
/// Retained history cells (about 17 MiB of levels). The engine evicts the oldest seconds first
/// when a deep book reaches this ceiling before the hour does.
const MAX_HISTORY_CELLS: usize = 720_000;
/// Ticks drawn either side of the window center; 2,049 rows stay under the engine's row cap.
const WINDOW_HALF_TICKS: i64 = 1_024;
/// The window is re-centered once the touch drifts this far from its center. Recorded history
/// stays in the engine book, so a rebuilt heatmap redraws every retained second.
const RECENTER_TICKS: i64 = WINDOW_HALF_TICKS / 2;
/// The size scale is rebuilt once the largest resting level outgrows it this many times.
const SIZE_SCALE_GROWTH: f64 = 2.0;
const HEATMAP_OPACITY: f32 = 0.72;
/// Distinguishes the heatmap's history stream from the current-book bars' snapshot stream.
const STREAM_KEY_SUFFIX: &str = "#depth-heatmap";

/// The engine depth stream recording one instrument's book history and the heatmap drawing it.
pub(super) struct DepthHeatmapState {
    identity: String,
    tick_size_bits: u64,
    stream: u64,
    heatmap: Option<DrawnHeatmap>,
    sequence: u64,
    last_timestamp_micros: i64,
}

#[derive(Clone, Copy, PartialEq)]
struct DrawnHeatmap {
    id: u64,
    center_tick: i64,
    maximum_size: f64,
    bid_rgba: [u8; 4],
    ask_rgba: [u8; 4],
}

impl AerisChartView {
    /// Records the instrument's current resting book at `timestamp_micros` (UTC) into the
    /// depth heatmap while that indicator is added and shown; otherwise the book is ignored.
    /// Both sides are ordered best price first. Timestamps that step backwards are held at the
    /// latest recorded time, so the history stays ordered.
    ///
    /// # Errors
    /// Returns an error for an invalid identity or tick size, or a book Aeris Charts rejects
    /// (off-grid or crossed). The recorded history is then removed rather than left stale.
    pub fn apply_depth_heatmap(
        &mut self,
        identity: &str,
        tick_size: f64,
        timestamp_micros: i64,
        mut bids: Vec<DepthLevel>,
        mut asks: Vec<DepthLevel>,
    ) -> Result<(), String> {
        if !self
            .order_flow_settings
            .depth_heatmap
            .is_some_and(|heatmap| heatmap.visible)
        {
            self.clear_depth_heatmap();
            return Ok(());
        }
        if identity.is_empty() || !tick_size.is_finite() || tick_size <= 0.0 {
            self.clear_depth_heatmap();
            return Err("depth heatmap identity and tick size must be valid".to_string());
        }
        if self.depth_heatmap_state.as_ref().is_some_and(|state| {
            state.identity != identity || state.tick_size_bits != tick_size.to_bits()
        }) {
            self.clear_depth_heatmap();
        }
        if self.depth_heatmap_state.is_none() {
            self.configure_depth_heatmap_stream(identity, tick_size)?;
        }
        let Some(state) = self.depth_heatmap_state.as_mut() else {
            return Err("depth heatmap was not configured".to_string());
        };
        state.sequence = state.sequence.saturating_add(1);
        state.last_timestamp_micros = state.last_timestamp_micros.max(timestamp_micros);
        let (stream, sequence, timestamp) =
            (state.stream, state.sequence, state.last_timestamp_micros);
        bids.truncate(MAX_LEVELS_PER_SIDE);
        asks.truncate(MAX_LEVELS_PER_SIDE);
        let touch = touch_tick(tick_size, &bids, &asks);
        let largest = bids
            .iter()
            .chain(&asks)
            .map(|level| level.size)
            .fold(0.0_f64, f64::max);
        let applied = self.engine.set_depth_snapshot(
            stream,
            DepthSnapshot {
                timestamp_micros: timestamp,
                sequence,
                bids,
                asks,
            },
        );
        if let Err(error) = applied {
            self.clear_depth_heatmap();
            return Err(error.to_string());
        }
        let drawn = touch.map_or(Ok(()), |touch| {
            self.sync_depth_heatmap_window(tick_size, touch, largest)
        });
        self.invalidate_series_frame();
        drawn.inspect_err(|_| self.clear_depth_heatmap())
    }

    /// Removes the recorded history and its heatmap, as when the instrument's book becomes
    /// unavailable. The indicator stays added and records the next book applied.
    pub fn clear_depth_heatmap(&mut self) {
        if let Some(state) = self.depth_heatmap_state.take() {
            self.engine.remove_depth_stream(state.stream);
            self.invalidate_series_frame();
        }
    }

    fn configure_depth_heatmap_stream(
        &mut self,
        identity: &str,
        tick_size: f64,
    ) -> Result<(), String> {
        let options = DepthOptions {
            tick_size,
            max_levels_per_side: MAX_LEVELS_PER_SIDE,
            history_bucket_micros: HISTORY_BUCKET_MICROS,
            max_history_buckets: MAX_HISTORY_BUCKETS,
            max_history_cells: MAX_HISTORY_CELLS,
            max_event_markers: 1,
        };
        let stream = self
            .engine
            .add_depth_stream(&format!("{identity}{STREAM_KEY_SUFFIX}"), options)
            .map_err(|error| error.to_string())?;
        self.depth_heatmap_state = Some(DepthHeatmapState {
            identity: identity.to_string(),
            tick_size_bits: tick_size.to_bits(),
            stream,
            heatmap: None,
            sequence: 0,
            last_timestamp_micros: i64::MIN,
        });
        Ok(())
    }

    /// Keeps the heatmap's fixed price window around the touch and its size scale above the
    /// resting book, rebuilding it from the recorded history only when either falls behind
    /// or the chart's bid and ask colors change.
    fn sync_depth_heatmap_window(
        &mut self,
        tick_size: f64,
        touch: i64,
        largest: f64,
    ) -> Result<(), String> {
        let (bid_rgba, ask_rgba) = self.depth_heatmap_colors();
        let Some(state) = self.depth_heatmap_state.as_ref() else {
            return Ok(());
        };
        let current = state.heatmap;
        let keep = current.is_some_and(|drawn| {
            touch.abs_diff(drawn.center_tick) <= RECENTER_TICKS.unsigned_abs()
                && largest <= drawn.maximum_size * SIZE_SCALE_GROWTH
                && drawn.bid_rgba == bid_rgba
                && drawn.ask_rgba == ask_rgba
        });
        if keep || largest <= 0.0 {
            return Ok(());
        }
        let stream = state.stream;
        let center_tick = touch;
        let price = |tick: i64| {
            tick.to_f64()
                .map(|tick| tick * tick_size)
                .ok_or_else(|| "depth heatmap window is out of range".to_string())
        };
        let price_min = price(center_tick.saturating_sub(WINDOW_HALF_TICKS))?;
        let price_max = price(center_tick.saturating_add(WINDOW_HALF_TICKS))?;
        if let Some(drawn) = current {
            self.engine.remove_depth_heatmap(drawn.id);
        }
        let id = self
            .engine
            .add_depth_heatmap(
                stream,
                DepthHeatmapOptions {
                    pane_index: 0,
                    price_min,
                    price_max,
                    minimum_size: 0.0,
                    maximum_size: largest,
                    bid_rgba,
                    ask_rgba,
                    opacity: HEATMAP_OPACITY,
                },
            )
            .map_err(|error| error.to_string())?;
        if let Some(state) = self.depth_heatmap_state.as_mut() {
            state.heatmap = Some(DrawnHeatmap {
                id,
                center_tick,
                maximum_size: largest,
                bid_rgba,
                ask_rgba,
            });
        }
        Ok(())
    }

    /// The chart's bullish (bid) and bearish (ask) colors, fully opaque; the heatmap's own
    /// opacity and size scale set how strongly each cell shows.
    fn depth_heatmap_colors(&self) -> ([u8; 4], [u8; 4]) {
        let layout = &self.engine.options.get().layout;
        let rgba = |css: &str, fallback: [u8; 4]| {
            ChartColor::parse_css(css)
                .map_or(fallback, |color| [color.r(), color.g(), color.b(), u8::MAX])
        };
        let defaults = DepthHeatmapOptions::default();
        (
            rgba(&layout.bullish_color, defaults.bid_rgba),
            rgba(&layout.bearish_color, defaults.ask_rgba),
        )
    }

    /// Removes the depth heatmap through the durable order-flow settings owner, so the removal
    /// persists and the indicator menu can add it back.
    pub(super) fn remove_depth_heatmap(&mut self) -> bool {
        if self.order_flow_settings.depth_heatmap.is_none() {
            return false;
        }
        self.set_order_flow_settings(OrderFlowSettings {
            depth_heatmap: None,
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    pub(super) fn set_depth_heatmap_visible(&mut self, visible: bool) -> bool {
        if self
            .order_flow_settings
            .depth_heatmap
            .is_none_or(|heatmap| heatmap.visible == visible)
        {
            return false;
        }
        self.set_order_flow_settings(OrderFlowSettings {
            depth_heatmap: Some(DepthHeatmapSettings { visible }),
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    /// The drawn heatmap's window center tick and size scale.
    #[cfg(test)]
    pub(super) fn depth_heatmap_window(&self) -> Option<(i64, f64)> {
        self.depth_heatmap_state
            .as_ref()?
            .heatmap
            .map(|drawn| (drawn.center_tick, drawn.maximum_size))
    }
}

/// The touch's tick: the mid of the best bid and ask, or whichever side is present.
fn touch_tick(tick_size: f64, bids: &[DepthLevel], asks: &[DepthLevel]) -> Option<i64> {
    let price = match (bids.first(), asks.first()) {
        (Some(bid), Some(ask)) => bid.price.midpoint(ask.price),
        (Some(level), None) | (None, Some(level)) => level.price,
        (None, None) => return None,
    };
    (price / tick_size).round().to_i64()
}
