use super::{
    AerisChartView, FootprintDisplayMode, OrderFlowAggregation, OrderFlowSettings, OrderFlowSweep,
    OrderFlowTrade,
};
use num_traits::ToPrimitive;

use aeris_charts_engine::{
    FootprintAggregationOptions, FootprintBarAggregation, FootprintCellMode,
    FootprintImbalanceOptions, FootprintTrade, FootprintVisualOptions, OrderFlowPresentation,
    OrderFlowPresentationOptions,
};

const SWEEP_AGGREGATION_WINDOW_MICROS: i64 = 100_000;
/// Recent price bars sampled for the automatic row size.
const AUTO_ROW_SAMPLE_BARS: usize = 64;

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
    first_ingestion_ordinal: Option<u64>,
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

    /// Sets the durable order-flow presentation and rebuilds only chart-derived state.
    ///
    /// # Errors
    /// Returns an error when the configured threshold is non-finite or negative.
    pub fn set_order_flow_settings(&mut self, settings: OrderFlowSettings) -> Result<bool, String> {
        if !settings.trade_bubble_minimum_volume.is_finite()
            || settings.trade_bubble_minimum_volume < 0.0
        {
            return Err("trade-bubble minimum volume must be finite and non-negative".to_string());
        }
        if self.order_flow_settings == settings {
            return Ok(false);
        }
        self.teardown_order_flow();
        self.order_flow_settings = settings;
        self.mark_user_state_changed();
        Ok(true)
    }

    /// Instrument ticks per footprint row currently drawn, resolving the automatic size.
    #[must_use]
    pub fn footprint_ticks_per_row(&self) -> Option<u32> {
        self.order_flow_state
            .as_ref()
            .filter(|state| state.presentation.footprint_series().is_some())
            .map(|state| state.presentation.ticks_per_row())
    }

    /// Returns the footprint to its candle history when the instrument has no known
    /// price increment. Footprint rows are keyed by the increment, so none are guessed.
    pub fn clear_order_flow_trades(&mut self) {
        self.teardown_order_flow();
    }

    /// Projects one runtime-authoritative bounded tape into the chart-owned
    /// footprint cache. Prefix eviction or session changes install a covering
    /// image; a stable retained prefix updates only the new suffix.
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
        if self.order_flow_state.is_none() {
            self.configure_order_flow(
                identity,
                provider_generation,
                aggregation,
                tick_size,
                trades,
            )?;
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
        let can_append = state.first_ingestion_ordinal == first
            && state.last_ingestion_ordinal.is_some()
            && state.last_ingestion_ordinal < last;
        let presentation = state.presentation;
        let prior_last = state.last_ingestion_ordinal;
        let converted = if can_append {
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
        self.engine
            .update_order_flow_presentation(presentation, converted, can_append)
            .map_err(|error| error.to_string())?;
        if let Some(state) = &mut self.order_flow_state {
            state.first_ingestion_ordinal = first;
            state.last_ingestion_ordinal = last;
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
        trades: &[OrderFlowTrade],
    ) -> Result<(), String> {
        let aggregation_options = FootprintAggregationOptions {
            tick_size,
            ticks_per_row: self.order_flow_settings.ticks_per_row,
            // Footprint bars share the price series' bar opens (weekly bars do not
            // open on the epoch's Thursday), so both presentations use one grid.
            bars: chart_aggregation(aggregation, self.product_bars.last_time())?,
            imbalance: FootprintImbalanceOptions::default(),
        };
        let trade_volumes = trades.iter().map(|trade| trade.volume).collect::<Vec<_>>();
        let presentation = self
            .engine
            .add_order_flow_presentation(
                identity,
                0,
                OrderFlowPresentationOptions {
                    aggregation: aggregation_options,
                    visual: FootprintVisualOptions {
                        cell_mode: cell_mode(self.order_flow_settings.display_mode),
                        ..FootprintVisualOptions::default()
                    },
                    recent_median_price_range: self
                        .product_bars
                        .recent_median_range(AUTO_ROW_SAMPLE_BARS),
                    show_footprint: self.chart_type == super::ChartType::Footprint,
                    show_cumulative_delta: self.order_flow_settings.show_cumulative_delta,
                    show_delta_histogram: self.order_flow_settings.show_delta_histogram,
                    show_trade_bubbles: self.order_flow_settings.show_trade_bubbles,
                    trade_bubble_minimum_volume: self
                        .order_flow_settings
                        .trade_bubble_minimum_volume,
                },
                &trade_volumes,
            )
            .map_err(|error| error.to_string())?;
        self.order_flow_state = Some(OrderFlowChartState {
            identity: identity.to_string(),
            provider_generation,
            aggregation,
            tick_size_bits: tick_size.to_bits(),
            first_ingestion_ordinal: None,
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

    fn order_flow_presentation_requested(&self) -> bool {
        self.chart_type == super::ChartType::Footprint
            || self.order_flow_settings.show_cumulative_delta
            || self.order_flow_settings.show_delta_histogram
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

fn adaptive_bubble_threshold(configured: f64, trades: &[OrderFlowTrade]) -> f64 {
    let volumes = trades.iter().map(|trade| trade.volume).collect::<Vec<_>>();
    aeris_charts_engine::adaptive_trade_bubble_threshold(configured, &volumes)
}

/// Groups consecutive aggressor-side prints that cross at least two price
/// levels inside the bounded sweep window. Provider order is preserved; no
/// opaque trade identifier is interpreted as sequence evidence.
#[must_use]
pub fn classify_order_flow_sweeps(
    trades: &[OrderFlowTrade],
    configured_minimum_volume: f64,
) -> Vec<OrderFlowSweep> {
    let threshold = adaptive_bubble_threshold(configured_minimum_volume, trades);
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
