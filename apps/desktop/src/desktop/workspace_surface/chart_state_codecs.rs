use super::super::{
    BigTradesFilter, BigTradesIntensity, BigTradesSettings, BigTradesSize, ChartAppearanceColor,
    ChartAppearanceSettings, ChartAxisAppearance, ChartColor, ChartDepthLevel, ChartIndicator,
    ChartIndicatorState, ChartInterval, ChartLegendOptions, ChartStudyInputRequirements,
    ChartStudyInputStream, ChartStudyPaneTarget, ChartStudyPlotKind, ChartStudyPointStyle,
    ChartStudyScaleTarget, ChartStudyThresholdRegion, CumulativeDeltaReset,
    CumulativeDeltaSettings, CumulativeDeltaStyle, DeltaHistogramSettings, DepthHeatmapSettings,
    FootprintDisplayMode, LiquidationStyle, LiquidationUnit, LiquidationsSettings, MarketStream,
    OpenInterestSettings, OpenInterestStyle, OrderBookDepthSettings, OrderFlowSettings,
    RejectedSavedPresentation, SessionProfileKind, SessionProfileSettings,
    SessionVolumeProfileSettings, StreamRequirements, StudyPaneTarget, StudyPlotKind,
    StudyPointStyle, StudyScaleTarget, StudyThresholdRegion, ToPrimitive, TpoCells, TpoSettings,
    VolumeAverageKind, VolumeAverageSettings, VolumePlacement, VolumeSettings,
    WorkspaceBigTradesState, WorkspaceChartAppearanceState, WorkspaceChartIndicatorState,
    WorkspaceChartLegendState, WorkspaceChartState, WorkspaceCumulativeDeltaState,
    WorkspaceDeltaHistogramState, WorkspaceOrderBookDepthState, WorkspaceOrderFlowSettingsState,
    WorkspaceSessionProfileState, WorkspaceVolumeSettingsState, chart_color_hex,
};
use aeris_localization::{tr, workspace};

/// Saved legend switches, or the default legend when an older workspace has none: everything on,
/// its indicator rows unfolded.
pub(in crate::desktop) fn restored_chart_legend(
    legend: Option<WorkspaceChartLegendState>,
) -> ChartLegendOptions {
    let legend = legend.unwrap_or_default();
    ChartLegendOptions {
        symbol: legend.show_symbol.unwrap_or(true),
        symbol_values: legend.show_symbol_values.unwrap_or(true),
        symbol_logo: legend.show_symbol_logo.unwrap_or(true),
        indicators: legend.show_indicators.unwrap_or(true),
        indicator_values: legend.show_indicator_values.unwrap_or(true),
        indicators_collapsed: legend.indicators_collapsed.unwrap_or(false),
    }
}

pub(in crate::desktop) fn persisted_chart_appearance(
    appearance: &ChartAppearanceSettings,
    axis: &ChartAxisAppearance,
) -> WorkspaceChartAppearanceState {
    WorkspaceChartAppearanceState {
        grid_visible: appearance.grid_visible,
        grid_color: persisted_appearance_color(&appearance.grid_color),
        grid_style: u32::from(appearance.grid_style),
        crosshair_color: persisted_appearance_color(&appearance.crosshair_color),
        crosshair_width: u32::from(appearance.crosshair_width),
        crosshair_style: u32::from(appearance.crosshair_style),
        up_color: persisted_appearance_color(&appearance.up_color),
        down_color: persisted_appearance_color(&appearance.down_color),
        wick_up_color: persisted_appearance_color(&appearance.wick_up_color),
        wick_down_color: persisted_appearance_color(&appearance.wick_down_color),
        border_up_color: persisted_appearance_color(&appearance.border_up_color),
        border_down_color: persisted_appearance_color(&appearance.border_down_color),
        wick_visible: appearance.wick_visible,
        border_visible: appearance.border_visible,
        open_visible: appearance.open_visible,
        thin_bars: appearance.thin_bars,
        line_color: persisted_appearance_color(&appearance.line_color),
        line_width: u32::from(appearance.line_width),
        line_style: u32::from(appearance.line_style),
        area_top_color: persisted_appearance_color(&appearance.area_top_color),
        area_bottom_color: persisted_appearance_color(&appearance.area_bottom_color),
        baseline_top_color: persisted_appearance_color(&appearance.baseline_top_color),
        baseline_bottom_color: persisted_appearance_color(&appearance.baseline_bottom_color),
        background_color: persisted_appearance_color(&appearance.background_color),
        text_color: persisted_appearance_color(&appearance.text_color),
        crosshair_style_custom: Some(
            appearance.crosshair_style != ChartAppearanceSettings::default().crosshair_style,
        ),
        axis_border_color: persisted_appearance_color(&axis.border_color),
        axis_text_color: persisted_appearance_color(&axis.text_color),
        axis_borders_visible: Some(axis.border_visible),
    }
}

/// The dashed style Aeris Charts drew the crosshair with by default when layouts were saved
/// without style provenance; those layouts stored it as a plain number.
const LEGACY_DEFAULT_CROSSHAIR_STYLE: u8 = 2;

/// A style the user chose survives; one that only recorded the chart default, including the
/// former dashed default in a legacy layout, follows the current default.
fn restored_crosshair_style(stored: u8, custom: Option<bool>) -> u8 {
    let custom = custom.unwrap_or(stored != LEGACY_DEFAULT_CROSSHAIR_STYLE);
    if custom {
        stored
    } else {
        ChartAppearanceSettings::default().crosshair_style
    }
}

pub(in crate::desktop) fn restored_chart_appearance(
    appearance: &WorkspaceChartAppearanceState,
) -> Option<ChartAppearanceSettings> {
    let grid_style = u8::try_from(appearance.grid_style).ok()?.min(4);
    let crosshair_width = u8::try_from(appearance.crosshair_width).ok()?.clamp(1, 4);
    let crosshair_style = restored_crosshair_style(
        u8::try_from(appearance.crosshair_style).ok()?.min(4),
        appearance.crosshair_style_custom,
    );
    let line_width = u8::try_from(appearance.line_width).ok()?.clamp(1, 4);
    let line_style = u8::try_from(appearance.line_style).ok()?.min(4);
    Some(ChartAppearanceSettings {
        background_color: restored_theme_color(&appearance.background_color),
        text_color: restored_theme_color(&appearance.text_color),
        grid_visible: appearance.grid_visible,
        grid_color: restored_theme_color(&appearance.grid_color),
        grid_style,
        crosshair_color: restored_theme_color(&appearance.crosshair_color),
        crosshair_width,
        crosshair_style,
        up_color: restored_theme_color(&appearance.up_color),
        down_color: restored_theme_color(&appearance.down_color),
        wick_up_color: restored_theme_color(&appearance.wick_up_color),
        wick_down_color: restored_theme_color(&appearance.wick_down_color),
        border_up_color: restored_theme_color(&appearance.border_up_color),
        border_down_color: restored_theme_color(&appearance.border_down_color),
        wick_visible: appearance.wick_visible,
        border_visible: appearance.border_visible,
        open_visible: appearance.open_visible,
        thin_bars: appearance.thin_bars,
        line_color: restored_theme_color(&appearance.line_color),
        line_width,
        line_style,
        area_top_color: restored_theme_color(&appearance.area_top_color),
        area_bottom_color: restored_theme_color(&appearance.area_bottom_color),
        baseline_top_color: restored_theme_color(&appearance.baseline_top_color),
        baseline_bottom_color: restored_theme_color(&appearance.baseline_bottom_color),
    })
}

pub(in crate::desktop) fn restored_chart_axis(
    appearance: &WorkspaceChartAppearanceState,
) -> ChartAxisAppearance {
    ChartAxisAppearance {
        border_color: restored_theme_color(&appearance.axis_border_color),
        text_color: restored_theme_color(&appearance.axis_text_color),
        border_visible: appearance.axis_borders_visible.unwrap_or(true),
    }
}

/// Durable marker for an appearance color that follows the active chart theme.
pub(in crate::desktop) const THEME_APPEARANCE_COLOR: &str = "theme";

fn persisted_appearance_color(color: &ChartAppearanceColor) -> String {
    match color {
        ChartAppearanceColor::Theme => THEME_APPEARANCE_COLOR.to_string(),
        ChartAppearanceColor::Custom(color) => color.clone(),
    }
}

/// A stored appearance color: schema 3 documents carry provenance (the theme marker or an
/// empty value follows the theme), and the migration chain canonicalizes every older document
/// before restore.
fn restored_theme_color(color: &str) -> ChartAppearanceColor {
    if color.is_empty() || color == THEME_APPEARANCE_COLOR {
        ChartAppearanceColor::Theme
    } else {
        ChartAppearanceColor::Custom(color.to_string())
    }
}

pub(super) fn persisted_big_trades(settings: BigTradesSettings) -> WorkspaceBigTradesState {
    let (filter, minimum_volume) = match settings.filter {
        BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Weak,
        } => (0, 0.0),
        BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Medium,
        } => (1, 0.0),
        BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Strong,
        } => (2, 0.0),
        BigTradesFilter::Fixed { minimum_volume } => (3, minimum_volume),
    };
    WorkspaceBigTradesState {
        filter,
        minimum_volume_bits: minimum_volume.to_bits(),
        size: match settings.size {
            BigTradesSize::Small => 0,
            BigTradesSize::Medium => 1,
            BigTradesSize::Large => 2,
        },
        show_volume: settings.show_volume,
        hidden: !settings.visible,
    }
}

pub(super) fn restored_big_trades(state: &WorkspaceBigTradesState) -> Option<BigTradesSettings> {
    let filter = match state.filter {
        0 => BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Weak,
        },
        1 => BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Medium,
        },
        2 => BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Strong,
        },
        3 => BigTradesFilter::Fixed {
            minimum_volume: Some(f64::from_bits(state.minimum_volume_bits))
                .filter(|volume| volume.is_finite() && *volume > 0.0)?,
        },
        _ => return None,
    };
    let size = match state.size {
        0 => BigTradesSize::Small,
        1 => BigTradesSize::Medium,
        2 => BigTradesSize::Large,
        _ => return None,
    };
    Some(BigTradesSettings {
        filter,
        size,
        show_volume: state.show_volume,
        visible: !state.hidden,
    })
}

/// The automatic intensity of a filter, or its fixed minimum volume.
pub(super) const fn big_trades_filter_draft(
    filter: BigTradesFilter,
) -> (Option<BigTradesIntensity>, Option<f64>) {
    match filter {
        BigTradesFilter::Auto { intensity } => (Some(intensity), None),
        BigTradesFilter::Fixed { minimum_volume } => (None, Some(minimum_volume)),
    }
}

pub(super) fn parse_big_trades_minimum_volume(text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|volume| volume.is_finite() && *volume > 0.0)
        .ok_or_else(|| tr!(workspace::MINIMUM_VOLUME_INVALID).to_string())
}

pub(super) fn parse_volume_average_length(text: &str) -> Result<u16, String> {
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|length| VolumeSettings::AVERAGE_LENGTHS.contains(length))
        .ok_or_else(|| {
            tr!(
                workspace::LENGTH_RANGE_INVALID,
                min = *VolumeSettings::AVERAGE_LENGTHS.start(),
                max = *VolumeSettings::AVERAGE_LENGTHS.end()
            )
        })
}

/// Editable text for an order volume. Sums of fractional prints carry binary rounding noise, so
/// the text is rounded unless that would erase a tiny but valid volume.
pub(super) fn big_trades_volume_text(volume: f64) -> String {
    let rounded = format!("{volume:.6}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed.parse::<f64>().is_ok_and(|value| value > 0.0) {
        trimmed.to_string()
    } else {
        volume.to_string()
    }
}

/// Chart depth levels drawn from an order-book frame, with the tick size their prices lie on,
/// or `None` while the book is recovering and has no trustworthy levels.
pub(super) fn chart_order_book_depth(
    frame: &aeris_market_data::OrderBookFrame,
) -> Option<(f64, Vec<ChartDepthLevel>, Vec<ChartDepthLevel>)> {
    use num_traits::ToPrimitive;
    if matches!(
        frame.state,
        aeris_market_data::OrderBookState::Recovering(_)
    ) {
        return None;
    }
    let price_divisor = 10_f64.powi(i32::from(frame.price_scale));
    let quantity_divisor = 10_f64.powi(i32::from(frame.quantity_scale));
    // Without a published increment every fixed-point price still lies on its scale's grid.
    let tick_size = frame.price_increment.unwrap_or(1).to_f64()? / price_divisor;
    let level = |level: &aeris_market_data::OrderBookColumnLevel| {
        let quantity = level.quantity.filter(|quantity| *quantity > 0)?;
        Some(ChartDepthLevel {
            price: level.price.to_f64()? / price_divisor,
            size: quantity.to_f64()? / quantity_divisor,
            order_count: level.order_count.filter(|count| *count > 0),
        })
    };
    let (bids, asks) = if frame.top_of_book_only {
        (
            frame.best_bid.iter().filter_map(level).collect(),
            frame.best_ask.iter().filter_map(level).collect(),
        )
    } else {
        (
            frame
                .rows
                .iter()
                .filter_map(|row| row.bid.as_ref())
                .filter_map(level)
                .collect(),
            frame
                .rows
                .iter()
                .filter_map(|row| row.ask.as_ref())
                .filter_map(level)
                .collect(),
        )
    };
    Some((tick_size, bids, asks))
}

pub(super) fn persisted_order_flow_settings(
    settings: OrderFlowSettings,
) -> WorkspaceOrderFlowSettingsState {
    WorkspaceOrderFlowSettingsState {
        display_mode: match settings.display_mode {
            FootprintDisplayMode::BidAsk => 0,
            FootprintDisplayMode::Total => 1,
            FootprintDisplayMode::Delta => 2,
            FootprintDisplayMode::ProfileInBar => 3,
            FootprintDisplayMode::VolumeLadder => 4,
            FootprintDisplayMode::HorizontalImbalance => 5,
            FootprintDisplayMode::BidAskHistogram => 6,
        },
        show_cumulative_delta: settings.show_cumulative_delta,
        show_delta_histogram: settings.show_delta_histogram,
        ticks_per_row: settings.ticks_per_row,
        study_visibility_revision: 1,
        big_trades: settings.big_trades.map(persisted_big_trades),
        order_book_depth: settings.order_book_depth.map(|book| book.visible),
        depth_heatmap: settings.depth_heatmap.map(|heatmap| heatmap.visible),
        open_interest: settings.open_interest.map(|oi| oi.visible),
        open_interest_options: settings.open_interest.map(|oi| {
            aeris_contracts::WorkspaceOpenInterestState {
                style: match oi.style {
                    OpenInterestStyle::Candles => 0,
                    OpenInterestStyle::Bars => 1,
                    OpenInterestStyle::Line => 2,
                    OpenInterestStyle::Area => 3,
                },
                line_color: persisted_chart_color(oi.line_color),
                up_color: persisted_chart_color(oi.up_color),
                down_color: persisted_chart_color(oi.down_color),
            }
        }),
        liquidations: settings.liquidations.map(|liq| liq.visible),
        liquidations_options: settings.liquidations.map(|liq| {
            aeris_contracts::WorkspaceLiquidationsState {
                style: match liq.style {
                    LiquidationStyle::Split => 0,
                    LiquidationStyle::Stacked => 1,
                    LiquidationStyle::Net => 2,
                },
                unit: match liq.unit {
                    LiquidationUnit::Usd => 0,
                    LiquidationUnit::Contracts => 1,
                },
                long_color: persisted_chart_color(liq.long_color),
                short_color: persisted_chart_color(liq.short_color),
            }
        }),
        cumulative_delta_style: match settings.cumulative_delta.style {
            CumulativeDeltaStyle::Line => 0,
            CumulativeDeltaStyle::Candles => 1,
        },
        cumulative_delta: Some(WorkspaceCumulativeDeltaState {
            reset: match settings.cumulative_delta.reset {
                CumulativeDeltaReset::Session => 0,
                CumulativeDeltaReset::Continuous => 1,
            },
            line_color: persisted_chart_color(settings.cumulative_delta.line_color),
            up_color: persisted_chart_color(settings.cumulative_delta.up_color),
            down_color: persisted_chart_color(settings.cumulative_delta.down_color),
        }),
        delta_histogram: Some(WorkspaceDeltaHistogramState {
            up_color: persisted_chart_color(settings.delta_histogram.up_color),
            down_color: persisted_chart_color(settings.delta_histogram.down_color),
        }),
        order_book_depth_options: settings.order_book_depth.map(|book| {
            WorkspaceOrderBookDepthState {
                levels_per_side: u32::from(book.levels_per_side),
                max_width: u32::from(book.max_width),
                opacity_percent: u32::from(book.opacity_percent),
                hide_sizes: !book.show_sizes,
                bid_color: persisted_chart_color(book.bid_color),
                ask_color: persisted_chart_color(book.ask_color),
            }
        }),
    }
}

/// An unset color persists as an empty string.
fn persisted_chart_color(color: Option<ChartColor>) -> String {
    color.map(chart_color_hex).unwrap_or_default()
}

/// A saved color that does not parse. The saved settings are rejected rather than guessed.
struct DamagedChartColor;

/// An empty string restores an unset color.
fn restored_chart_color(value: &str) -> Result<Option<ChartColor>, DamagedChartColor> {
    if value.is_empty() {
        return Ok(None);
    }
    ChartColor::parse_css(value)
        .map(Some)
        .ok_or(DamagedChartColor)
}

/// Older workspaces without CVD options restore the session reset in default colors.
fn restored_cumulative_delta(
    style: u32,
    state: Option<&WorkspaceCumulativeDeltaState>,
) -> Option<CumulativeDeltaSettings> {
    let style = match style {
        0 => CumulativeDeltaStyle::Line,
        1 => CumulativeDeltaStyle::Candles,
        _ => return None,
    };
    let Some(state) = state else {
        return Some(CumulativeDeltaSettings {
            style,
            ..CumulativeDeltaSettings::default()
        });
    };
    Some(CumulativeDeltaSettings {
        style,
        reset: match state.reset {
            0 => CumulativeDeltaReset::Session,
            1 => CumulativeDeltaReset::Continuous,
            _ => return None,
        },
        line_color: restored_chart_color(&state.line_color).ok()?,
        up_color: restored_chart_color(&state.up_color).ok()?,
        down_color: restored_chart_color(&state.down_color).ok()?,
    })
}

fn restored_delta_histogram(
    state: Option<&WorkspaceDeltaHistogramState>,
) -> Option<DeltaHistogramSettings> {
    let Some(state) = state else {
        return Some(DeltaHistogramSettings::default());
    };
    Some(DeltaHistogramSettings {
        up_color: restored_chart_color(&state.up_color).ok()?,
        down_color: restored_chart_color(&state.down_color).ok()?,
    })
}

/// Older workspaces carry no bar options and restore the defaults.
fn restored_order_book_depth(
    visible: bool,
    state: Option<&WorkspaceOrderBookDepthState>,
) -> Option<OrderBookDepthSettings> {
    let Some(state) = state else {
        return Some(OrderBookDepthSettings {
            visible,
            ..OrderBookDepthSettings::default()
        });
    };
    let settings = OrderBookDepthSettings {
        visible,
        levels_per_side: u16::try_from(state.levels_per_side).ok()?,
        max_width: u16::try_from(state.max_width).ok()?,
        opacity_percent: u8::try_from(state.opacity_percent).ok()?,
        show_sizes: !state.hide_sizes,
        bid_color: restored_chart_color(&state.bid_color).ok()?,
        ask_color: restored_chart_color(&state.ask_color).ok()?,
    };
    settings.is_valid().then_some(settings)
}

/// Older workspaces carry no open-interest style/color options and restore candles in default colors.
fn restored_open_interest(
    visible: bool,
    state: Option<&aeris_contracts::WorkspaceOpenInterestState>,
) -> Option<OpenInterestSettings> {
    let Some(state) = state else {
        return Some(OpenInterestSettings {
            visible,
            ..OpenInterestSettings::default()
        });
    };
    Some(OpenInterestSettings {
        visible,
        style: match state.style {
            0 => OpenInterestStyle::Candles,
            1 => OpenInterestStyle::Bars,
            2 => OpenInterestStyle::Line,
            3 => OpenInterestStyle::Area,
            _ => return None,
        },
        line_color: restored_chart_color(&state.line_color).ok()?,
        up_color: restored_chart_color(&state.up_color).ok()?,
        down_color: restored_chart_color(&state.down_color).ok()?,
    })
}

/// Older workspaces carry no liquidations style/unit/color options and restore split USD histograms.
fn restored_liquidations(
    visible: bool,
    state: Option<&aeris_contracts::WorkspaceLiquidationsState>,
) -> Option<LiquidationsSettings> {
    let Some(state) = state else {
        return Some(LiquidationsSettings {
            visible,
            ..LiquidationsSettings::default()
        });
    };
    Some(LiquidationsSettings {
        visible,
        style: match state.style {
            0 => LiquidationStyle::Split,
            1 => LiquidationStyle::Stacked,
            2 => LiquidationStyle::Net,
            _ => return None,
        },
        unit: match state.unit {
            0 => LiquidationUnit::Usd,
            1 => LiquidationUnit::Contracts,
            _ => return None,
        },
        long_color: restored_chart_color(&state.long_color).ok()?,
        short_color: restored_chart_color(&state.short_color).ok()?,
    })
}

pub(super) fn restored_order_flow_settings(
    settings: &WorkspaceOrderFlowSettingsState,
) -> Option<OrderFlowSettings> {
    let display_mode = match settings.display_mode {
        0 => FootprintDisplayMode::BidAsk,
        1 => FootprintDisplayMode::Total,
        2 => FootprintDisplayMode::Delta,
        3 => FootprintDisplayMode::ProfileInBar,
        4 => FootprintDisplayMode::VolumeLadder,
        5 => FootprintDisplayMode::HorizontalImbalance,
        6 => FootprintDisplayMode::BidAskHistogram,
        _ => return None,
    };
    let big_trades = match &settings.big_trades {
        Some(state) => Some(restored_big_trades(state)?),
        None => None,
    };
    // Tag 9 records whether the order book is added and, when it is, whether it is shown.
    let order_book_depth = match settings.order_book_depth {
        Some(visible) => Some(restored_order_book_depth(
            visible,
            settings.order_book_depth_options.as_ref(),
        )?),
        None => None,
    };
    let open_interest = match settings.open_interest {
        Some(visible) => Some(restored_open_interest(
            visible,
            settings.open_interest_options.as_ref(),
        )?),
        None => None,
    };
    let liquidations = match settings.liquidations {
        Some(visible) => Some(restored_liquidations(
            visible,
            settings.liquidations_options.as_ref(),
        )?),
        None => None,
    };
    Some(OrderFlowSettings {
        display_mode,
        show_cumulative_delta: settings.study_visibility_revision >= 1
            && settings.show_cumulative_delta,
        cumulative_delta: restored_cumulative_delta(
            settings.cumulative_delta_style,
            settings.cumulative_delta.as_ref(),
        )?,
        show_delta_histogram: settings.study_visibility_revision >= 1
            && settings.show_delta_histogram,
        delta_histogram: restored_delta_histogram(settings.delta_histogram.as_ref())?,
        big_trades,
        order_book_depth,
        depth_heatmap: settings
            .depth_heatmap
            .map(|visible| DepthHeatmapSettings { visible }),
        open_interest,
        liquidations,
        ticks_per_row: settings.ticks_per_row,
        ..OrderFlowSettings::default()
    })
}

impl RejectedSavedPresentation {
    pub(super) fn of(state: &WorkspaceChartState) -> Self {
        Self {
            appearance: state
                .appearance
                .as_ref()
                .is_some_and(|appearance| restored_chart_appearance(appearance).is_none()),
            order_flow: state
                .order_flow
                .as_ref()
                .is_some_and(|settings| restored_order_flow_settings(settings).is_none()),
        }
    }

    pub(super) const fn any(self) -> bool {
        self.appearance || self.order_flow
    }
}

pub(super) const fn chart_study_plot(plot: StudyPlotKind) -> ChartStudyPlotKind {
    match plot {
        StudyPlotKind::Line
        | StudyPlotKind::StepLine
        | StudyPlotKind::Circles
        | StudyPlotKind::Cross => ChartStudyPlotKind::Line,
        StudyPlotKind::Histogram | StudyPlotKind::Columns => ChartStudyPlotKind::Histogram,
        StudyPlotKind::Area => ChartStudyPlotKind::Area,
    }
}

pub(super) const fn chart_study_pane(pane: StudyPaneTarget) -> ChartStudyPaneTarget {
    match pane {
        StudyPaneTarget::Price => ChartStudyPaneTarget::Price,
        StudyPaneTarget::Dedicated { group } => ChartStudyPaneTarget::Dedicated { group },
    }
}

pub(super) const fn chart_study_scale(scale: StudyScaleTarget) -> ChartStudyScaleTarget {
    match scale {
        StudyScaleTarget::Primary => ChartStudyScaleTarget::Primary,
        StudyScaleTarget::Left => ChartStudyScaleTarget::Left,
        StudyScaleTarget::Overlay => ChartStudyScaleTarget::Overlay,
    }
}

pub(super) const fn chart_study_point_style(style: StudyPointStyle) -> ChartStudyPointStyle {
    match style {
        StudyPointStyle::Uniform | StudyPointStyle::PerPoint => ChartStudyPointStyle::Uniform,
        StudyPointStyle::MomentumHistogram => ChartStudyPointStyle::MomentumHistogram,
    }
}

pub(super) const fn chart_study_input_requirements(
    requirements: StreamRequirements,
) -> ChartStudyInputRequirements {
    let mut mapped = ChartStudyInputRequirements::NONE;
    if requirements.contains(MarketStream::Bars) {
        mapped = mapped.with(ChartStudyInputStream::Bars);
    }
    if requirements.contains(MarketStream::Trades) {
        mapped = mapped.with(ChartStudyInputStream::Trades);
    }
    if requirements.contains(MarketStream::Quotes) {
        mapped = mapped.with(ChartStudyInputStream::Quotes);
    }
    if requirements.contains(MarketStream::Depth) {
        mapped = mapped.with(ChartStudyInputStream::Depth);
    }
    mapped
}

pub(super) fn chart_study_threshold(
    region: Option<StudyThresholdRegion>,
) -> Option<ChartStudyThresholdRegion> {
    region.and_then(|region| {
        Some(ChartStudyThresholdRegion {
            lower: region.lower.mantissa.to_f64()? / 10_f64.powi(i32::from(region.lower.scale)),
            upper: region.upper.mantissa.to_f64()? / 10_f64.powi(i32::from(region.upper.scale)),
        })
    })
}

pub(in crate::desktop) const fn runtime_managed_indicator(indicator: ChartIndicator) -> bool {
    matches!(
        indicator,
        ChartIndicator::Sma
            | ChartIndicator::Ema
            | ChartIndicator::Bollinger
            | ChartIndicator::Vwap
            | ChartIndicator::Rsi
            | ChartIndicator::Macd
            | ChartIndicator::Stochastic
            | ChartIndicator::Atr
    )
}

pub(super) fn persisted_legacy_indicator_states(
    states: impl IntoIterator<Item = ChartIndicatorState>,
) -> Vec<WorkspaceChartIndicatorState> {
    states
        .into_iter()
        .filter(|state| !runtime_managed_indicator(state.indicator))
        .map(|state| WorkspaceChartIndicatorState {
            kind: state.indicator.identifier().to_string(),
            visible: state.visible,
            volume: state.volume.as_ref().map(persisted_volume_settings),
            session_profile: state
                .session_profile
                .map(persisted_session_profile_settings),
        })
        .collect()
}

/// The chart-owned indicators of a saved document; runtime studies and unknown kinds are left
/// out.
pub(super) fn restored_legacy_indicator_states(
    states: &[WorkspaceChartIndicatorState],
) -> Vec<ChartIndicatorState> {
    states
        .iter()
        .filter_map(|item| {
            ChartIndicator::from_identifier(&item.kind).map(|indicator| ChartIndicatorState {
                indicator,
                visible: item.visible,
                volume: item
                    .volume
                    .as_ref()
                    .filter(|_| indicator == ChartIndicator::Volume)
                    .and_then(restored_volume_settings),
                session_profile: SessionProfileKind::of(indicator).and_then(|kind| {
                    item.session_profile
                        .as_ref()
                        .and_then(|state| restored_session_profile_settings(kind, state))
                }),
            })
        })
        .filter(|state| !runtime_managed_indicator(state.indicator))
        .take(ChartIndicator::CATALOG_LEN)
        .collect()
}

pub(super) fn persisted_volume_settings(settings: &VolumeSettings) -> WorkspaceVolumeSettingsState {
    WorkspaceVolumeSettingsState {
        up_color: settings.up_color.clone().unwrap_or_default(),
        down_color: settings.down_color.clone().unwrap_or_default(),
        average_enabled: settings.average.enabled,
        average_kind: match settings.average.kind {
            VolumeAverageKind::Simple => 0,
            VolumeAverageKind::Exponential => 1,
        },
        average_length: u32::from(settings.average.length),
        average_color: settings.average.color.clone().unwrap_or_default(),
        placement: match settings.placement {
            VolumePlacement::PricePane => 0,
            VolumePlacement::OwnPane => 1,
        },
    }
}

/// An empty color follows the Aeris Charts default. A value outside the accepted range is
/// rejected so a damaged layout restores the volume defaults instead of a partial guess.
pub(super) fn restored_volume_settings(
    state: &WorkspaceVolumeSettingsState,
) -> Option<VolumeSettings> {
    let color = |value: &str| (!value.is_empty()).then(|| value.to_string());
    let kind = match state.average_kind {
        0 => VolumeAverageKind::Simple,
        1 => VolumeAverageKind::Exponential,
        _ => return None,
    };
    let placement = match state.placement {
        0 => VolumePlacement::PricePane,
        1 => VolumePlacement::OwnPane,
        _ => return None,
    };
    let length = u16::try_from(state.average_length)
        .ok()
        .filter(|length| VolumeSettings::AVERAGE_LENGTHS.contains(length))?;
    Some(VolumeSettings {
        up_color: color(&state.up_color),
        down_color: color(&state.down_color),
        average: VolumeAverageSettings {
            enabled: state.average_enabled,
            kind,
            length,
            color: color(&state.average_color),
        },
        placement,
    })
}

pub(super) fn persisted_session_profile_settings(
    settings: SessionProfileSettings,
) -> WorkspaceSessionProfileState {
    match settings {
        SessionProfileSettings::Volume(volume) => WorkspaceSessionProfileState {
            value_area_percent: u32::from(volume.value_area_percent),
            sessions_per_profile: u32::from(volume.sessions_per_profile),
            row_count: u32::from(volume.row_count),
            show_developing: volume.show_developing,
            extend_naked_levels: volume.extend_naked_levels,
            ..WorkspaceSessionProfileState::default()
        },
    }
}

/// The saved settings of one session profile kind. A value outside its accepted range is
/// rejected so a damaged layout restores that profile's defaults instead of a partial guess.
pub(super) fn restored_session_profile_settings(
    kind: SessionProfileKind,
    state: &WorkspaceSessionProfileState,
) -> Option<SessionProfileSettings> {
    let settings = match kind {
        SessionProfileKind::Volume => {
            SessionProfileSettings::Volume(SessionVolumeProfileSettings {
                sessions_per_profile: u8::try_from(state.sessions_per_profile).ok()?,
                row_count: u16::try_from(state.row_count).ok()?,
                value_area_percent: u8::try_from(state.value_area_percent).ok()?,
                show_developing: state.show_developing,
                extend_naked_levels: state.extend_naked_levels,
            })
        }
    };
    settings.is_valid().then_some(settings)
}

/// The timeframes a TPO chart loads, one per TPO period, in minutes.
const TPO_PERIOD_INTERVALS: [(u16, ChartInterval); 6] = [
    (5, ChartInterval::Minute5),
    (15, ChartInterval::Minute15),
    (30, ChartInterval::Minute30),
    (60, ChartInterval::Hour1),
    (120, ChartInterval::Hour2),
    (240, ChartInterval::Hour4),
];

/// The timeframe a TPO of `period_minutes` loads among those the provider offers (all of them
/// when it publishes no list): the longest that is not longer than the period, so every bar
/// falls inside one letter, else the shortest. `None` when it offers none.
pub(super) fn tpo_period_interval(
    period_minutes: u16,
    offered: &[ChartInterval],
) -> Option<ChartInterval> {
    let offered = || {
        TPO_PERIOD_INTERVALS
            .into_iter()
            .filter(|(_, interval)| offered.is_empty() || offered.contains(interval))
    };
    offered()
        .filter(|(minutes, _)| *minutes <= period_minutes)
        .max_by_key(|(minutes, _)| *minutes)
        .or_else(|| offered().min_by_key(|(minutes, _)| *minutes))
        .map(|(_, interval)| interval)
}

/// The TPO period, in minutes, a timeframe choice on a TPO chart stands for: its length,
/// clamped to the TPO periods. Tick and calendar-month bars stand for none.
pub(super) fn tpo_period_minutes(interval: ChartInterval) -> Option<u16> {
    let aeris_market_data::ChartAggregation::FixedSeconds(seconds) = interval.aggregation() else {
        return None;
    };
    let minutes = u16::try_from(seconds.get() / 60).unwrap_or(u16::MAX);
    TPO_PERIOD_INTERVALS
        .into_iter()
        .min_by_key(|(period, _)| period.abs_diff(minutes))
        .map(|(period, _)| period)
}

/// The TPO chart type's settings in the `tpo_*` and value-area fields they have always used.
pub(in crate::desktop) fn persisted_tpo_settings(
    settings: TpoSettings,
) -> WorkspaceSessionProfileState {
    WorkspaceSessionProfileState {
        tpo_cells: match settings.cells {
            TpoCells::Letters => 0,
            TpoCells::Blocks => 1,
        },
        tpo_period_minutes: u32::from(settings.period_minutes),
        tpo_initial_balance_periods: u32::from(settings.initial_balance_periods),
        value_area_percent: u32::from(settings.value_area_percent),
        ..WorkspaceSessionProfileState::default()
    }
}

/// Saved TPO settings; a value outside its accepted range is rejected so a damaged layout
/// opens the TPO on its defaults instead of a partial guess.
pub(in crate::desktop) fn restored_tpo_settings(
    state: &WorkspaceSessionProfileState,
) -> Option<TpoSettings> {
    let settings = TpoSettings {
        cells: match state.tpo_cells {
            0 => TpoCells::Letters,
            1 => TpoCells::Blocks,
            _ => return None,
        },
        period_minutes: u16::try_from(state.tpo_period_minutes).ok()?,
        value_area_percent: u8::try_from(state.value_area_percent).ok()?,
        initial_balance_periods: u8::try_from(state.tpo_initial_balance_periods).ok()?,
    };
    settings.is_valid().then_some(settings)
}
