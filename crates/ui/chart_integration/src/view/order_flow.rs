use super::{
    FootprintDisplayMode, NucleusChartView, OrderFlowAggregation, OrderFlowSettings,
    OrderFlowSweep, OrderFlowTrade,
};
use aeris_charts_engine::{
    FootprintAggregationOptions, FootprintBarAggregation, FootprintCellMode,
    FootprintImbalanceOptions, FootprintSeriesOptions, FootprintTrade, FootprintVisualOptions,
    TradeBubbleOptions, TradeStudyOptions,
};

const TRADE_BUBBLE_CAPACITY: usize = 2_048;
const SWEEP_AGGREGATION_WINDOW_MICROS: i64 = 100_000;

pub(super) struct OrderFlowChartState {
    identity: String,
    provider_generation: u64,
    aggregation: OrderFlowAggregation,
    tick_size_bits: u64,
    first_ingestion_ordinal: Option<u64>,
    last_ingestion_ordinal: Option<u64>,
    footprint_series: u32,
    cumulative_delta_series: Option<u32>,
    delta_series: Option<u32>,
}

impl NucleusChartView {
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
        if self.chart_type != super::ChartType::Footprint {
            return Ok(());
        }
        if identity.is_empty() || !tick_size.is_finite() || tick_size <= 0.0 {
            return Err("order-flow identity and tick size must be valid".to_string());
        }
        let requires_configuration = self.order_flow_state.is_none();
        if requires_configuration {
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
            || state.tick_size_bits != tick_size.to_bits()
        {
            return Err("order-flow tape identity changed without replacing the chart".to_string());
        }

        let first = trades.first().map(|trade| trade.ingestion_ordinal);
        let last = trades.last().map(|trade| trade.ingestion_ordinal);
        let can_append = state.first_ingestion_ordinal == first
            && state.last_ingestion_ordinal.is_some()
            && state.last_ingestion_ordinal < last;
        let footprint_series = state.footprint_series;
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
        if can_append {
            self.engine
                .update_footprint_trades(footprint_series, converted)
                .map_err(|error| error.to_string())?;
        } else {
            self.engine
                .set_footprint_trades(footprint_series, converted)
                .map_err(|error| error.to_string())?;
        }
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
            bars: chart_aggregation(aggregation)?,
            imbalance: FootprintImbalanceOptions::default(),
        };
        let stream = self
            .engine
            .add_trade_stream(identity, aggregation_options)
            .map_err(|error| error.to_string())?;
        let visual = FootprintVisualOptions {
            cell_mode: cell_mode(self.order_flow_settings.display_mode),
            ..FootprintVisualOptions::default()
        };
        let footprint_series = self
            .engine
            .add_footprint_series(FootprintSeriesOptions {
                aggregation: aggregation_options,
                visual,
            })
            .map_err(|error| error.to_string())?;
        self.engine
            .bind_footprint_series_to_stream(footprint_series, stream)
            .map_err(|error| error.to_string())?;
        self.engine.set_series_visible(0, false);

        let cumulative_delta_series = if self.order_flow_settings.show_cumulative_delta {
            let pane = self
                .engine
                .add_pane(false)
                .ok_or_else(|| "order-flow pane capacity is exhausted".to_string())?;
            Some(
                self.engine
                    .add_cvd_series(stream, pane, TradeStudyOptions::default())
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let delta_series = if self.order_flow_settings.show_delta_histogram {
            let pane = self
                .engine
                .add_pane(false)
                .ok_or_else(|| "order-flow pane capacity is exhausted".to_string())?;
            Some(
                self.engine
                    .add_delta_series(stream, pane)
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        if self.order_flow_settings.show_trade_bubbles {
            self.engine
                .add_trade_bubbles(
                    stream,
                    footprint_series,
                    TradeBubbleOptions {
                        minimum_volume: adaptive_bubble_threshold(
                            self.order_flow_settings.trade_bubble_minimum_volume,
                            trades,
                        ),
                        max_markers: TRADE_BUBBLE_CAPACITY,
                        aggregation_window_micros: SWEEP_AGGREGATION_WINDOW_MICROS,
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        self.order_flow_state = Some(OrderFlowChartState {
            identity: identity.to_string(),
            provider_generation,
            aggregation,
            tick_size_bits: tick_size.to_bits(),
            first_ingestion_ordinal: None,
            last_ingestion_ordinal: None,
            footprint_series,
            cumulative_delta_series,
            delta_series,
        });
        Ok(())
    }

    pub(super) fn teardown_order_flow(&mut self) {
        let Some(state) = self.order_flow_state.take() else {
            return;
        };
        if let Some(series) = state.cumulative_delta_series {
            self.engine.remove_series(series);
        }
        if let Some(series) = state.delta_series {
            self.engine.remove_series(series);
        }
        self.engine.remove_series(state.footprint_series);
        self.engine.set_series_visible(0, true);
        self.invalidate_series_layout();
    }
}

fn chart_aggregation(aggregation: OrderFlowAggregation) -> Result<FootprintBarAggregation, String> {
    match aggregation {
        OrderFlowAggregation::TimeMicros(interval_micros) if interval_micros > 0 => {
            Ok(FootprintBarAggregation::Time {
                interval_micros,
                anchor_micros: 0,
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
    if configured > 0.0 {
        return configured;
    }
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
