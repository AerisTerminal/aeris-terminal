use super::{
    AerisChartView, BigTradesFilter, BigTradesSettings, CumulativeDeltaReset,
    CumulativeDeltaSettings, FootprintDisplayMode, OrderFlowAggregation, OrderFlowApplyError,
    OrderFlowDefaultColors, OrderFlowSettings, OrderFlowTapeChange, OrderFlowTrade,
    chart_color_hex,
};
use aeris_design_system::AerisTheme;
use num_traits::ToPrimitive;

use aeris_charts_engine::{
    BigTradesOptions, FootprintAggregationOptions, FootprintBarAggregation, FootprintCellMode,
    FootprintImbalanceOptions, FootprintTrade, FootprintVisualOptions, OrderFlowPresentation,
    OrderFlowPresentationOptions, TradeStudyOptions,
};

/// Tape-derived study panes backed by the chart's shared order-flow stream.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum OrderFlowStudy {
    CumulativeDelta,
    Delta,
}

impl OrderFlowStudy {
    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::CumulativeDelta => "CVD",
            Self::Delta => "Delta",
        }
    }
}

pub(super) struct OrderFlowChartState {
    identity: String,
    provider_generation: u64,
    aggregation: OrderFlowAggregation,
    tick_size_bits: u64,
    /// Newest applied print ordinal. Observation only; the host's
    /// Append/Replace decision never reads it.
    applied_through: Option<u64>,
    /// Set once a covering window is installed; an append requires it.
    window_installed: bool,
    pub(super) presentation: OrderFlowPresentation,
}

impl OrderFlowChartState {
    pub(super) const fn study_series(&self, study: OrderFlowStudy) -> Option<u32> {
        match study {
            OrderFlowStudy::CumulativeDelta => self.presentation.cumulative_delta_series(),
            OrderFlowStudy::Delta => self.presentation.delta_series(),
        }
    }
}

impl AerisChartView {
    #[must_use]
    pub fn order_flow_settings(&self) -> OrderFlowSettings {
        self.order_flow_settings
    }

    /// Sets the durable order-flow presentation. The drawn presentation is reconfigured in
    /// place, so its footprint history survives row, display, study, and big-trades changes.
    ///
    /// # Errors
    /// Returns an error when a fixed big-trades minimum volume is not finite and positive.
    pub fn set_order_flow_settings(&mut self, settings: OrderFlowSettings) -> Result<bool, String> {
        if let Some(BigTradesSettings {
            filter: BigTradesFilter::Fixed { minimum_volume },
            ..
        }) = settings.big_trades
            && !(minimum_volume.is_finite() && minimum_volume > 0.0)
        {
            return Err("big-trades minimum volume must be finite and positive".to_string());
        }
        if settings
            .order_book_depth
            .is_some_and(|book| !book.is_valid())
        {
            return Err("order-book settings are out of range".to_string());
        }
        if self.order_flow_settings == settings {
            return Ok(false);
        }
        let book_changed = self.order_flow_settings.order_book_depth != settings.order_book_depth;
        self.order_flow_settings = settings;
        if settings.order_book_depth.is_some_and(|book| book.visible) {
            if book_changed {
                self.sync_order_book_depth_options();
            }
        } else {
            self.clear_order_book_depth();
        }
        if !settings
            .depth_heatmap
            .is_some_and(|heatmap| heatmap.visible)
        {
            self.clear_depth_heatmap();
        }
        self.reconfigure_order_flow();
        self.mark_user_state_changed();
        Ok(true)
    }

    /// The colors the order-flow indicators draw where their settings leave a color unset.
    #[must_use]
    pub fn order_flow_default_colors(&self) -> OrderFlowDefaultColors {
        let layout = &self.engine.options.get().layout;
        let hex = |css: &str| super::drawing_color_hex(css).unwrap_or_else(|| css.to_string());
        let footprint = FootprintVisualOptions::default();
        OrderFlowDefaultColors {
            cumulative_delta_line: chart_color_hex(aeris_charts_engine::DEFAULT_LINE_COLOR),
            bullish: hex(&layout.bullish_color),
            bearish: hex(&layout.bearish_color),
            delta_positive: chart_color_hex(footprint.positive_delta_color),
            delta_negative: chart_color_hex(footprint.negative_delta_color),
        }
    }

    /// The minimum order volume the drawn big-trades indicator applies; `None` before it is
    /// drawn and while its automatic filter is still sampling.
    #[must_use]
    pub fn big_trades_threshold(&self) -> Option<f64> {
        let id = self.drawn_big_trades()?;
        self.engine.big_trades_snapshot(id)?.threshold
    }

    /// Instrument ticks per footprint row currently drawn, resolving the automatic size.
    #[must_use]
    pub fn footprint_ticks_per_row(&self) -> Option<u32> {
        self.order_flow_state
            .as_ref()
            .filter(|state| state.presentation.footprint_series().is_some())
            .map(|state| state.presentation.ticks_per_row())
    }

    /// Returns the footprint to candle history when its trade tape is unavailable.
    pub fn clear_order_flow_trades(&mut self) {
        self.teardown_order_flow();
    }

    /// Applies one incremental tape publication to the chart-owned footprint
    /// cache. The pane's order-flow presenter owns tape continuity from the
    /// runtime's `Reset`/`Append` stream: a [`OrderFlowTapeChange::ReplaceWindow`]
    /// installs the covering retained window, replacing the span it covers while
    /// bars older than the window are kept; an [`OrderFlowTapeChange::Append`]
    /// extends the installed window with prints newer than every applied print.
    ///
    /// Once drawn, the presentation keeps following the tape while no order-flow view is
    /// shown, so switching back to a footprint finds its history.
    ///
    /// # Errors
    /// Returns [`OrderFlowApplyError::AwaitingCoveringWindow`] when an append
    /// arrives before this stream's covering window is installed, and
    /// [`OrderFlowApplyError::Failed`] for invalid identity, aggregation,
    /// fixed-point projection, or chart resource exhaustion.
    pub fn apply_order_flow_trades(
        &mut self,
        identity: &str,
        provider_generation: u64,
        aggregation: OrderFlowAggregation,
        tick_size: f64,
        trades: &[OrderFlowTrade],
        change: OrderFlowTapeChange,
    ) -> Result<(), OrderFlowApplyError> {
        if !self.order_flow_presentation_requested() && self.order_flow_state.is_none() {
            return Ok(());
        }
        if identity.is_empty() || !tick_size.is_finite() || tick_size <= 0.0 {
            return Err(OrderFlowApplyError::Failed(
                "order-flow identity and tick size must be valid".to_string(),
            ));
        }
        // Refreshed provider metadata can correct the price increment of the
        // same instrument; footprint rows are keyed by it, so rebuild them.
        if self
            .order_flow_state
            .as_ref()
            .is_some_and(|state| state.tick_size_bits != tick_size.to_bits())
        {
            self.teardown_order_flow();
        }
        let mut reconfigured = false;
        if self.order_flow_state.is_none() {
            self.configure_order_flow(identity, provider_generation, aggregation, tick_size)
                .map_err(OrderFlowApplyError::Failed)?;
            reconfigured = true;
        }
        let state = self.order_flow_state.as_mut().ok_or_else(|| {
            OrderFlowApplyError::Failed("order-flow chart state was not configured".to_string())
        })?;
        if state.identity != identity
            || state.provider_generation > provider_generation
            || state.aggregation != aggregation
        {
            return Err(OrderFlowApplyError::Failed(
                "order-flow tape identity changed without replacing the chart".to_string(),
            ));
        }
        match change {
            OrderFlowTapeChange::ReplaceWindow => {
                // A reconnected provider session restarts its tape and
                // ingestion ordinals; its covering window replaces the span.
                state.provider_generation = provider_generation;
                state.window_installed = true;
            }
            OrderFlowTapeChange::Append => {
                if reconfigured
                    || !state.window_installed
                    || state.provider_generation != provider_generation
                {
                    return Err(OrderFlowApplyError::AwaitingCoveringWindow);
                }
            }
        }
        let converted = trades
            .iter()
            .map(order_flow_trade)
            .collect::<Result<Vec<_>, _>>()
            .map_err(OrderFlowApplyError::Failed)?;
        let presentation = state.presentation;
        match change {
            OrderFlowTapeChange::ReplaceWindow => self
                .engine
                .replace_order_flow_window(presentation, converted),
            OrderFlowTapeChange::Append => self
                .engine
                .update_order_flow_presentation(presentation, converted, true)
                .map(|_| ()),
        }
        .map_err(|error| OrderFlowApplyError::Failed(error.to_string()))?;
        if let Some(state) = &mut self.order_flow_state
            && let Some(newest) = trades.last().map(|trade| trade.ingestion_ordinal)
        {
            // A covering window replaces the stream, so its newest print is the
            // chart's newest; an append only ever moves it forward.
            state.applied_through = match change {
                OrderFlowTapeChange::ReplaceWindow => Some(newest),
                OrderFlowTapeChange::Append => Some(
                    state
                        .applied_through
                        .map_or(newest, |applied| applied.max(newest)),
                ),
            };
        }
        // The engine re-lays out on its own series revision; a covering window
        // lays out once, while a tip append only repaints the frame so the
        // price axis does not shrink and jitter while the tape streams.
        match change {
            OrderFlowTapeChange::ReplaceWindow => self.invalidate_series_layout(),
            OrderFlowTapeChange::Append => self.invalidate_series_frame(),
        }
        Ok(())
    }

    /// The newest tape ordinal this chart applied for exactly this stream.
    /// Observation only: the host's Append/Replace decision never reads it.
    #[must_use]
    pub fn order_flow_applied_through(
        &self,
        identity: &str,
        provider_generation: u64,
        aggregation: OrderFlowAggregation,
        tick_size: f64,
    ) -> Option<u64> {
        self.order_flow_state
            .as_ref()
            .filter(|state| {
                state.identity == identity
                    && state.provider_generation == provider_generation
                    && state.aggregation == aggregation
                    && state.tick_size_bits == tick_size.to_bits()
            })
            .and_then(|state| state.applied_through)
    }

    fn configure_order_flow(
        &mut self,
        identity: &str,
        provider_generation: u64,
        aggregation: OrderFlowAggregation,
        tick_size: f64,
    ) -> Result<(), String> {
        let options = self.order_flow_presentation_options(FootprintAggregationOptions {
            tick_size,
            ticks_per_row: self.order_flow_settings.ticks_per_row,
            // Footprint bars share the price series' bar opens (weekly bars do not
            // open on the epoch's Thursday), so both presentations use one grid.
            bars: chart_aggregation(aggregation, self.product_bars.last_time())?,
            imbalance: FootprintImbalanceOptions::default(),
        });
        let presentation = self
            .engine
            .add_order_flow_presentation(identity, 0, options)
            .map_err(|error| error.to_string())?;
        self.order_flow_state = Some(OrderFlowChartState {
            identity: identity.to_string(),
            provider_generation,
            aggregation,
            tick_size_bits: tick_size.to_bits(),
            applied_through: None,
            window_installed: false,
            presentation,
        });
        Ok(())
    }

    /// Brings the drawn presentation to the durable settings and chart type on its existing
    /// stream, keeping every footprint bar. A presentation the engine cannot reconfigure is
    /// removed and rebuilt from the next tape.
    pub(super) fn reconfigure_order_flow(&mut self) {
        let Some(stream) = self
            .order_flow_state
            .as_ref()
            .map(|state| state.presentation.trade_stream())
        else {
            return;
        };
        let Some(current) = self
            .engine
            .trade_stream(stream)
            .map(aeris_charts_engine::FootprintAggregator::options)
        else {
            self.teardown_order_flow();
            return;
        };
        let options = self.order_flow_presentation_options(FootprintAggregationOptions {
            ticks_per_row: self.order_flow_settings.ticks_per_row,
            ..current
        });
        let reconfigured = self.order_flow_state.as_mut().is_some_and(|state| {
            self.engine
                .reconfigure_order_flow_presentation(&mut state.presentation, options)
                .is_ok()
        });
        if reconfigured {
            self.invalidate_series_layout();
        } else {
            self.teardown_order_flow();
        }
    }

    fn order_flow_presentation_options(
        &self,
        aggregation: FootprintAggregationOptions,
    ) -> OrderFlowPresentationOptions {
        OrderFlowPresentationOptions {
            aggregation,
            visual: footprint_visual_options(cell_mode(self.order_flow_settings.display_mode)),
            show_footprint: self.chart_type == super::ChartType::Footprint,
            show_cumulative_delta: self.order_flow_settings.show_cumulative_delta,
            cumulative_delta: cumulative_delta_options(self.order_flow_settings.cumulative_delta),
            show_delta_histogram: self.order_flow_settings.show_delta_histogram,
            delta_histogram: TradeStudyOptions {
                up_color: self.order_flow_settings.delta_histogram.up_color,
                down_color: self.order_flow_settings.delta_histogram.down_color,
                ..TradeStudyOptions::default()
            },
            big_trades: self
                .order_flow_settings
                .big_trades
                .map(|settings| big_trades_options(settings, &self.platform)),
        }
    }

    pub(super) fn teardown_order_flow(&mut self) {
        let Some(state) = self.order_flow_state.take() else {
            return;
        };
        let _ = self
            .engine
            .remove_order_flow_presentation(state.presentation);
        self.invalidate_series_layout();
    }

    /// The footprint series, which shares the product price legend row.
    pub(super) fn footprint_series_id(&self) -> Option<u32> {
        self.order_flow_state
            .as_ref()
            .and_then(|state| state.presentation.footprint_series())
    }

    /// Whether the footprint has received any trade tape to draw.
    pub(super) fn has_footprint_bars(&self) -> bool {
        self.footprint_series_id()
            .and_then(|id| self.engine.footprint_bar(id, 0))
            .is_some()
    }

    /// Whether a tape-derived study pane is currently drawn.
    pub(super) fn has_order_flow_study(&self, study: OrderFlowStudy) -> bool {
        self.order_flow_state
            .as_ref()
            .is_some_and(|state| state.study_series(study).is_some())
    }

    /// Order-flow study owning a chart series, if any.
    pub(super) fn order_flow_study_for_series(&self, series: u32) -> Option<OrderFlowStudy> {
        let state = self.order_flow_state.as_ref()?;
        [OrderFlowStudy::CumulativeDelta, OrderFlowStudy::Delta]
            .into_iter()
            .find(|study| state.study_series(*study) == Some(series))
    }

    pub(super) fn set_order_flow_study_visible(
        &mut self,
        study: OrderFlowStudy,
        visible: bool,
    ) -> bool {
        let Some(series) = self
            .order_flow_state
            .as_ref()
            .and_then(|state| state.study_series(study))
        else {
            return false;
        };
        let changed = self
            .engine
            .series_visible(series)
            .is_some_and(|value| value != visible);
        if changed {
            self.engine.set_series_visible(series, visible);
            self.invalidate_series_layout();
        }
        changed
    }

    /// Removes a study pane through the durable order-flow settings owner, so the
    /// removal persists and the indicator menu can add it back.
    pub(super) fn remove_order_flow_study(&mut self, study: OrderFlowStudy) -> bool {
        let mut settings = self.order_flow_settings;
        match study {
            OrderFlowStudy::CumulativeDelta => settings.show_cumulative_delta = false,
            OrderFlowStudy::Delta => settings.show_delta_histogram = false,
        }
        self.set_order_flow_settings(settings).unwrap_or(false)
    }

    /// Removes the big-trades indicator through the durable order-flow settings owner, so the
    /// removal persists and the indicator menu can add it back.
    pub(super) fn remove_big_trades(&mut self) -> bool {
        if self.order_flow_settings.big_trades.is_none() {
            return false;
        }
        self.set_order_flow_settings(OrderFlowSettings {
            big_trades: None,
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    pub(super) fn set_big_trades_visible(&mut self, visible: bool) -> bool {
        let Some(big_trades) = self
            .order_flow_settings
            .big_trades
            .filter(|big_trades| big_trades.visible != visible)
        else {
            return false;
        };
        self.set_order_flow_settings(OrderFlowSettings {
            big_trades: Some(BigTradesSettings {
                visible,
                ..big_trades
            }),
            ..self.order_flow_settings
        })
        .unwrap_or(false)
    }

    /// Restyles the drawn big-trades indicator from the durable settings and current theme.
    pub(super) fn sync_big_trades_options(&mut self) {
        let (Some(settings), Some(id)) =
            (self.order_flow_settings.big_trades, self.drawn_big_trades())
        else {
            return;
        };
        let options = big_trades_options(settings, &self.platform);
        if self.engine.big_trades_options(id) != Some(&options)
            && self.engine.set_big_trades_options(id, options).is_ok()
        {
            self.invalidate_series_frame();
        }
    }

    pub(super) fn drawn_big_trades(&self) -> Option<aeris_charts_engine::NativePrimitiveId> {
        self.order_flow_state
            .as_ref()
            .and_then(|state| state.presentation.big_trades())
    }

    fn order_flow_presentation_requested(&self) -> bool {
        self.chart_type == super::ChartType::Footprint
            || self.order_flow_settings.show_cumulative_delta
            || self.order_flow_settings.show_delta_histogram
            || self.order_flow_settings.big_trades.is_some()
    }
}

const fn cumulative_delta_options(settings: CumulativeDeltaSettings) -> TradeStudyOptions {
    TradeStudyOptions {
        cumulative_delta_reset: match settings.reset {
            CumulativeDeltaReset::Session => aeris_charts_engine::CumulativeDeltaReset::Session,
            CumulativeDeltaReset::Continuous => {
                aeris_charts_engine::CumulativeDeltaReset::Continuous
            }
        },
        anchor_timestamp_micros: None,
        style: settings.style,
        line_color: settings.line_color,
        up_color: settings.up_color,
        down_color: settings.down_color,
    }
}

/// Engine options for durable big-trades settings: translucent token fills outlined with the
/// chart direction colors, labelled in the chart's own text color.
fn big_trades_options(settings: BigTradesSettings, theme: &AerisTheme) -> BigTradesOptions {
    let colors = theme.colors;
    BigTradesOptions {
        filter: settings.filter,
        size: settings.size,
        show_volume: settings.show_volume,
        visible: settings.visible,
        buy_color: colors.buy_bubble.css_rgba(),
        sell_color: colors.sell_bubble.css_rgba(),
        buy_border_color: colors.bullish.css_rgba(),
        sell_border_color: colors.bearish.css_rgba(),
        ..BigTradesOptions::default()
    }
}

/// Footprint cells keep the engine's order-flow tokens, which both themes share.
fn footprint_visual_options(cell_mode: FootprintCellMode) -> FootprintVisualOptions {
    FootprintVisualOptions {
        cell_mode,
        ..FootprintVisualOptions::default()
    }
}

fn chart_aggregation(
    aggregation: OrderFlowAggregation,
    price_bar_open_seconds: Option<f64>,
) -> Result<FootprintBarAggregation, String> {
    match aggregation {
        OrderFlowAggregation::TimeMicros(interval_micros) if interval_micros > 0 => {
            let anchor_micros = price_bar_open_seconds
                .map(|seconds| seconds * 1_000_000.0)
                .filter(|micros| micros.is_finite() && micros.abs() < 9.0e15)
                .and_then(|micros| micros.round().to_i64())
                .unwrap_or(0);
            Ok(FootprintBarAggregation::Time {
                interval_micros,
                anchor_micros,
            })
        }
        OrderFlowAggregation::Trades(trades_per_bar) if trades_per_bar > 0 => {
            Ok(FootprintBarAggregation::Trades { trades_per_bar })
        }
        OrderFlowAggregation::Volume(volume_per_bar)
            if volume_per_bar.is_finite() && volume_per_bar > 0.0 =>
        {
            Ok(FootprintBarAggregation::Volume { volume_per_bar })
        }
        _ => Err("order-flow aggregation must be finite and positive".to_string()),
    }
}

const fn cell_mode(mode: FootprintDisplayMode) -> FootprintCellMode {
    match mode {
        FootprintDisplayMode::BidAsk => FootprintCellMode::BidAsk,
        FootprintDisplayMode::Total => FootprintCellMode::Total,
        FootprintDisplayMode::Delta => FootprintCellMode::Delta,
        FootprintDisplayMode::ProfileInBar => FootprintCellMode::ProfileInBar,
        FootprintDisplayMode::VolumeLadder => FootprintCellMode::VolumeLadder,
        FootprintDisplayMode::HorizontalImbalance => FootprintCellMode::HorizontalImbalance,
        FootprintDisplayMode::BidAskHistogram => FootprintCellMode::BidAskHistogram,
    }
}

fn order_flow_trade(trade: &OrderFlowTrade) -> Result<FootprintTrade, String> {
    if !trade.price.is_finite() || !trade.volume.is_finite() || trade.volume <= 0.0 {
        return Err("order-flow trade contains a non-finite price or volume".to_string());
    }
    Ok(FootprintTrade {
        timestamp_micros: trade.timestamp_micros,
        price: trade.price,
        volume: trade.volume,
        aggressor: trade.aggressor,
        bid: None,
        ask: None,
        sequence: Some(trade.ingestion_ordinal),
        // Provider trade identifiers are opaque strings. Never hash them into
        // false numeric sequence/correction semantics.
        trade_id: None,
        conditions: 0,
        session_id: trade.session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_time_bars_share_the_price_series_bar_opens() {
        const WEEK_MICROS: u64 = 7 * 24 * 60 * 60 * 1_000_000;
        // Monday 2026-09-21 00:00 UTC; the Unix epoch fell on a Thursday.
        let monday_open = 1_790_035_200.0;
        let FootprintBarAggregation::Time {
            interval_micros,
            anchor_micros,
        } = chart_aggregation(
            OrderFlowAggregation::TimeMicros(WEEK_MICROS),
            Some(monday_open),
        )
        .expect("weekly aggregation")
        else {
            panic!("time aggregation expected");
        };
        assert_eq!(interval_micros, WEEK_MICROS);
        assert_eq!(anchor_micros, 1_790_035_200_000_000);
        assert!(matches!(
            chart_aggregation(OrderFlowAggregation::TimeMicros(WEEK_MICROS), None),
            Ok(FootprintBarAggregation::Time {
                anchor_micros: 0,
                ..
            })
        ));
    }
}
