use super::{AerisChartView, OrderFlowSettings};
use aeris_charts_engine::{DepthLevel, DepthOptions, DepthProfileOptions, DepthSnapshot};

/// Levels kept per side of the drawn book; deeper levels of a larger book are dropped.
const MAX_LEVELS_PER_SIDE: usize = 1_024;

/// The bars draw only the current book. A fixed snapshot time keeps the engine from rolling
/// depth history buckets the chart never shows.
const SNAPSHOT_TIMESTAMP_MICROS: i64 = 0;

/// The engine depth stream holding one instrument's current book for the right-edge bars.
pub(super) struct OrderBookDepthState {
    identity: String,
    tick_size_bits: u64,
    pub(super) stream: u64,
    sequence: u64,
}

impl AerisChartView {
    /// Draws the instrument's current resting book as bars at the price pane's right edge
    /// while the order-book indicator is added and shown; otherwise the book is ignored.
    /// Each call replaces the whole drawn book. Both sides are ordered best price first.
    ///
    /// # Errors
    /// Returns an error for an invalid identity or tick size, or a book Aeris Charts rejects
    /// (off-grid or crossed). The drawn book is then removed rather than left stale.
    pub fn apply_order_book_depth(
        &mut self,
        identity: &str,
        tick_size: f64,
        mut bids: Vec<DepthLevel>,
        mut asks: Vec<DepthLevel>,
    ) -> Result<(), String> {
        if self.order_flow_settings.order_book_depth != Some(true) {
            self.clear_order_book_depth();
            return Ok(());
        }
        if identity.is_empty() || !tick_size.is_finite() || tick_size <= 0.0 {
            self.clear_order_book_depth();
            return Err("order-book identity and tick size must be valid".to_string());
        }
        if self.order_book_depth_state.as_ref().is_some_and(|state| {
            state.identity != identity || state.tick_size_bits != tick_size.to_bits()
        }) {
            self.clear_order_book_depth();
        }
        if self.order_book_depth_state.is_none() {
            self.configure_order_book_depth(identity, tick_size)?;
        }
        let Some((stream, sequence)) = self.order_book_depth_state.as_mut().map(|state| {
            state.sequence = state.sequence.saturating_add(1);
            (state.stream, state.sequence)
        }) else {
            return Err("order-book depth was not configured".to_string());
        };
        bids.truncate(MAX_LEVELS_PER_SIDE);
        asks.truncate(MAX_LEVELS_PER_SIDE);
        let applied = self.engine.set_depth_snapshot(
            stream,
            DepthSnapshot {
                timestamp_micros: SNAPSHOT_TIMESTAMP_MICROS,
                sequence,
                bids,
                asks,
            },
        );
        self.invalidate_series_frame();
        applied.map_err(|error| {
            self.clear_order_book_depth();
            error.to_string()
        })
    }

    /// Removes the drawn book, as when the instrument's book becomes unavailable. The
    /// indicator stays added and draws the next book applied.
    pub fn clear_order_book_depth(&mut self) {
        if let Some(state) = self.order_book_depth_state.take() {
            self.engine.remove_depth_stream(state.stream);
            self.invalidate_series_frame();
        }
    }

    fn configure_order_book_depth(&mut self, identity: &str, tick_size: f64) -> Result<(), String> {
        let options = DepthOptions {
            tick_size,
            max_levels_per_side: MAX_LEVELS_PER_SIDE,
            max_history_buckets: 1,
            max_history_cells: MAX_LEVELS_PER_SIDE * 2,
            max_event_markers: 1,
            ..DepthOptions::default()
        };
        let stream = self
            .engine
            .add_depth_stream(identity, options)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self
            .engine
            .add_depth_profile(stream, DepthProfileOptions::default())
        {
            self.engine.remove_depth_stream(stream);
            return Err(error.to_string());
        }
        self.order_book_depth_state = Some(OrderBookDepthState {
            identity: identity.to_string(),
            tick_size_bits: tick_size.to_bits(),
            stream,
            sequence: 0,
        });
        Ok(())
    }

    /// Removes the order-book indicator through the durable order-flow settings owner, so the
    /// removal persists and the indicator menu can add it back.
    pub(super) fn remove_order_book_depth(&mut self) -> bool {
        if self.order_flow_settings.order_book_depth.is_none() {
            return false;
        }
        self.set_order_flow_settings(OrderFlowSettings {
            order_book_depth: None,
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    pub(super) fn set_order_book_depth_visible(&mut self, visible: bool) -> bool {
        if self
            .order_flow_settings
            .order_book_depth
            .is_none_or(|shown| shown == visible)
        {
            return false;
        }
        self.set_order_flow_settings(OrderFlowSettings {
            order_book_depth: Some(visible),
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }
}
