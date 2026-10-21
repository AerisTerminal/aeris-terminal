use super::{AerisChartView, ChartColor, OrderBookDepthSettings, OrderFlowSettings};
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
    pub(super) profile: u64,
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
        let Some(settings) = self
            .order_flow_settings
            .order_book_depth
            .filter(|book| book.visible)
        else {
            self.clear_order_book_depth();
            return Ok(());
        };
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
            self.configure_order_book_depth(identity, tick_size, settings)?;
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

    fn configure_order_book_depth(
        &mut self,
        identity: &str,
        tick_size: f64,
        settings: OrderBookDepthSettings,
    ) -> Result<(), String> {
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
        let profile = match self
            .engine
            .add_depth_profile(stream, depth_profile_options(settings))
        {
            Ok(profile) => profile,
            Err(error) => {
                self.engine.remove_depth_stream(stream);
                return Err(error.to_string());
            }
        };
        self.order_book_depth_state = Some(OrderBookDepthState {
            identity: identity.to_string(),
            tick_size_bits: tick_size.to_bits(),
            stream,
            profile,
            sequence: 0,
        });
        Ok(())
    }

    /// Restyles the drawn bars from the durable settings in place, keeping the drawn book.
    /// Bars the engine refuses to restyle are removed and drawn again from the next book.
    pub(super) fn sync_order_book_depth_options(&mut self) {
        let (Some(settings), Some(profile)) = (
            self.order_flow_settings.order_book_depth,
            self.order_book_depth_state
                .as_ref()
                .map(|state| state.profile),
        ) else {
            return;
        };
        if self
            .engine
            .set_depth_profile_options(profile, depth_profile_options(settings))
            .is_ok()
        {
            self.invalidate_series_frame();
        } else {
            self.clear_order_book_depth();
        }
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
        let Some(book) = self
            .order_flow_settings
            .order_book_depth
            .filter(|book| book.visible != visible)
        else {
            return false;
        };
        self.set_order_flow_settings(OrderFlowSettings {
            order_book_depth: Some(OrderBookDepthSettings { visible, ..book }),
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }
}

fn depth_profile_options(settings: OrderBookDepthSettings) -> DepthProfileOptions {
    let rgba = |color: ChartColor| [color.r(), color.g(), color.b(), color.a()];
    DepthProfileOptions {
        pane_index: 0,
        levels_per_side: usize::from(settings.levels_per_side),
        max_width: f64::from(settings.max_width),
        bid_rgba: settings.bid_color.map(rgba),
        ask_rgba: settings.ask_color.map(rgba),
        opacity: f32::from(settings.opacity_percent) / 100.0,
        show_sizes: settings.show_sizes,
    }
}
