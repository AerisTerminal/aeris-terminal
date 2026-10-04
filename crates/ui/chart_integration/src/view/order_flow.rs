use super::{
    AerisChartView, BigTradesFilter, BigTradesSettings, ChartTheme, FootprintDisplayMode,
    OrderFlowAggregation, OrderFlowSettings, OrderFlowSweep, OrderFlowTrade, platform_theme,
};
use aeris_charts_render::color::Color;
use num_traits::ToPrimitive;

use aeris_charts_engine::{
    BigTradesOptions, FootprintAggregationOptions, FootprintBarAggregation, FootprintCellMode,
    FootprintImbalanceOptions, FootprintTrade, FootprintVisualOptions, OrderFlowPresentation,
    OrderFlowPresentationOptions,
};

const SWEEP_AGGREGATION_WINDOW_MICROS: i64 = 100_000;

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
    last_ingestion_ordinal: Option<u64>,
    presentation: OrderFlowPresentation,
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

    /// Sets the durable order-flow presentation and rebuilds only chart-derived state. A change
    /// confined to the settings of an existing big-trades indicator restyles it in place.
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
        if self.order_flow_settings == settings {
            return Ok(false);
        }
        let restyle_only = self.order_flow_settings.big_trades.is_some()
            && settings.big_trades.is_some()
            && OrderFlowSettings {
                big_trades: None,
                ..self.order_flow_settings
            } == OrderFlowSettings {
                big_trades: None,
                ..settings
            };
        self.order_flow_settings = settings;
        if restyle_only {
            self.sync_big_trades_options();
        } else {
            self.teardown_order_flow();
        }
        self.mark_user_state_changed();
        Ok(true)
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

    /// Invalidates an incremental prefix after a canonical history/correction rewrite.
    pub fn invalidate_order_flow_prefix(&mut self) {
        if let Some(state) = &mut self.order_flow_state {
            state.last_ingestion_ordinal = None;
        }
    }

    /// Returns the footprint to candle history when its trade tape is unavailable.
    pub fn clear_order_flow_trades(&mut self) {
        self.teardown_order_flow();
    }

    /// Projects one runtime-authoritative bounded tape into the chart-owned
    /// footprint cache. While the runtime's sliding window continues the applied
    /// tape without a gap, only its new suffix is sent, so the chart keeps bars
    /// whose trades the runtime has since evicted. A gap, rewrite, or session
    /// change installs a covering image of the current window.
    ///
    /// # Errors
    /// Returns an error for invalid identity, aggregation, fixed-point projection,
    /// or chart resource exhaustion.
    pub fn apply_order_flow_trades(
        &mut self,
        identity: &str,
        provider_generation: u64,
        aggregation: OrderFlowAggregation,
        tick_size: f64,
        trades: &[OrderFlowTrade],
    ) -> Result<(), String> {
        if !self.order_flow_presentation_requested() {
            return Ok(());
        }
        if identity.is_empty() || !tick_size.is_finite() || tick_size <= 0.0 {
            return Err("order-flow identity and tick size must be valid".to_string());
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
        if self.order_flow_state.as_ref().is_some_and(|state| {
            state.identity == identity && state.provider_generation < provider_generation
        }) {
            self.teardown_order_flow();
        }
        if self.order_flow_state.is_none() {
            self.configure_order_flow(identity, provider_generation, aggregation, tick_size)?;
        }
        let state = self
            .order_flow_state
            .as_ref()
            .ok_or_else(|| "order-flow chart state was not configured".to_string())?;
        if state.identity != identity
            || state.provider_generation != provider_generation
            || state.aggregation != aggregation
        {
            return Err("order-flow tape identity changed without replacing the chart".to_string());
        }

        let first = trades.first().map(|trade| trade.ingestion_ordinal);
        let last = trades.last().map(|trade| trade.ingestion_ordinal);
        let can_append = state
            .last_ingestion_ordinal
            .is_some_and(|applied| match (first, last) {
                (Some(first), Some(last)) => first <= applied.saturating_add(1) && last >= applied,
                _ => true,
            });
        let presentation = state.presentation;
        let prior_last = state.last_ingestion_ordinal;
        let mut converted = if can_append {
            let suffix_start = trades.partition_point(|trade| {
                prior_last.is_some_and(|last| trade.ingestion_ordinal <= last)
            });
            trades[suffix_start..]
                .iter()
                .map(order_flow_trade)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            trades
                .iter()
                .map(order_flow_trade)
                .collect::<Result<Vec<_>, _>>()?
        };
        if !can_append {
            converted.sort_by_key(|trade| trade.timestamp_micros);
        }
        self.engine
            .update_order_flow_presentation(presentation, converted, can_append)
            .map_err(|error| error.to_string())?;
        if let Some(state) = &mut self.order_flow_state {
            state.last_ingestion_ordinal = if can_append {
                last.max(prior_last)
            } else {
                last
            };
        }
        self.invalidate_series_layout();
        Ok(())
    }

    fn configure_order_flow(
        &mut self,
        identity: &str,
        provider_generation: u64,
        aggregation: OrderFlowAggregation,
        tick_size: f64,
    ) -> Result<(), String> {
        let aggregation_options = FootprintAggregationOptions {
            tick_size,
            ticks_per_row: self.order_flow_settings.ticks_per_row,
            // Footprint bars share the price series' bar opens (weekly bars do not
            // open on the epoch's Thursday), so both presentations use one grid.
            bars: chart_aggregation(aggregation, self.product_bars.last_time())?,
            imbalance: FootprintImbalanceOptions::default(),
        };
        let presentation = self
            .engine
            .add_order_flow_presentation(
                identity,
                0,
                OrderFlowPresentationOptions {
                    aggregation: aggregation_options,
                    visual: footprint_visual_options(
                        cell_mode(self.order_flow_settings.display_mode),
                        self.theme,
                    ),
                    show_footprint: self.chart_type == super::ChartType::Footprint,
                    show_cumulative_delta: self.order_flow_settings.show_cumulative_delta,
                    show_delta_histogram: self.order_flow_settings.show_delta_histogram,
                    big_trades: self
                        .order_flow_settings
                        .big_trades
                        .map(|settings| big_trades_options(settings, self.theme)),
                },
            )
            .map_err(|error| error.to_string())?;
        self.order_flow_state = Some(OrderFlowChartState {
            identity: identity.to_string(),
            provider_generation,
            aggregation,
            tick_size_bits: tick_size.to_bits(),
            last_ingestion_ordinal: None,
            presentation,
        });
        Ok(())
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
            .and_then(|id| self.engine.footprint_bars(id))
            .is_some_and(|bars| !bars.is_empty())
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
        let options = big_trades_options(settings, self.theme);
        if self.engine.big_trades_options(id) != Some(&options)
            && self.engine.set_big_trades_options(id, options).is_ok()
        {
            self.invalidate_series_frame();
        }
    }

    /// Retokenizes the drawn footprint's cell colors for the current theme.
    pub(super) fn sync_footprint_visual_options(&mut self) {
        let Some(id) = self.footprint_series_id() else {
            return;
        };
        let Some(mut options) = self.engine.footprint_series_options(id) else {
            return;
        };
        let mut visual = footprint_visual_options(options.visual.cell_mode, self.theme);
        visual.adaptive_rows = options.visual.adaptive_rows;
        if visual == options.visual {
            return;
        }
        options.visual = visual;
        if self
            .engine
            .apply_footprint_series_options(id, options)
            .is_ok()
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

/// Engine options for durable big-trades settings: translucent token fills outlined with the
/// chart direction colors, labelled in the chart's own text color.
fn big_trades_options(settings: BigTradesSettings, theme: ChartTheme) -> BigTradesOptions {
    let colors = platform_theme(theme).colors;
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

/// Footprint cells in the chart direction tokens: sells on `bearish`, buys on `bullish`. The
/// engine derives the track, volume-bar, and imbalance shades from these hues.
fn footprint_visual_options(
    cell_mode: FootprintCellMode,
    theme: ChartTheme,
) -> FootprintVisualOptions {
    let colors = platform_theme(theme).colors;
    let defaults = FootprintVisualOptions::default();
    let token = |value: String, fallback: Color| Color::parse_css(&value).unwrap_or(fallback);
    let bullish = token(colors.bullish.css_rgba(), defaults.ask_color);
    let bearish = token(colors.bearish.css_rgba(), defaults.bid_color);
    FootprintVisualOptions {
        cell_mode,
        bid_color: bearish,
        ask_color: bullish,
        positive_delta_color: bullish,
        negative_delta_color: bearish,
        stacked_bid_color: bearish,
        stacked_ask_color: bullish,
        ..defaults
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

/// The 90th percentile of the positive print volumes, so a sweep is a run that together outsizes
/// nine in ten recent prints. `f64::MAX` when there is no positive print.
fn sweep_volume_threshold(trades: &[OrderFlowTrade]) -> f64 {
    let mut volumes = trades
        .iter()
        .map(|trade| trade.volume)
        .filter(|volume| volume.is_finite() && *volume > 0.0)
        .collect::<Vec<_>>();
    if volumes.is_empty() {
        return f64::MAX;
    }
    let index = volumes.len().saturating_mul(9).saturating_sub(1) / 10;
    volumes.select_nth_unstable_by(index, f64::total_cmp);
    volumes[index]
}

/// Groups consecutive aggressor-side prints that cross at least two price
/// levels inside the bounded sweep window and outsize the recent prints.
/// Provider order is preserved; no opaque trade identifier is interpreted as
/// sequence evidence.
#[must_use]
pub fn classify_order_flow_sweeps(trades: &[OrderFlowTrade]) -> Vec<OrderFlowSweep> {
    let threshold = sweep_volume_threshold(trades);
    let mut sweeps = Vec::new();
    let mut start = 0;
    while start < trades.len() {
        let first = &trades[start];
        if first.aggressor == aeris_charts_engine::AggressorSide::Unknown {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        let mut total_volume = first.volume;
        let mut levels = 1_u32;
        let mut previous_price = first.price;
        while let Some(next) = trades.get(end) {
            let previous = &trades[end - 1];
            let within_window = next.timestamp_micros >= previous.timestamp_micros
                && next
                    .timestamp_micros
                    .saturating_sub(previous.timestamp_micros)
                    <= SWEEP_AGGREGATION_WINDOW_MICROS;
            let price_continues = match first.aggressor {
                aeris_charts_engine::AggressorSide::Buy => next.price >= previous_price,
                aeris_charts_engine::AggressorSide::Sell => next.price <= previous_price,
                aeris_charts_engine::AggressorSide::Unknown => false,
            };
            if next.aggressor != first.aggressor || !within_window || !price_continues {
                break;
            }
            if next.price.to_bits() != previous_price.to_bits() {
                levels = levels.saturating_add(1);
            }
            total_volume += next.volume;
            previous_price = next.price;
            end += 1;
        }
        if levels >= 2 && total_volume >= threshold {
            let terminal = &trades[end - 1];
            sweeps.push(OrderFlowSweep {
                first_ingestion_ordinal: first.ingestion_ordinal,
                last_ingestion_ordinal: terminal.ingestion_ordinal,
                timestamp_micros: terminal.timestamp_micros,
                terminal_price: terminal.price,
                total_volume,
                price_levels: levels,
                aggressor: first.aggressor,
            });
        }
        start = end.max(start + 1);
    }
    sweeps
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
        session_id: Some(trade.session_id),
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
