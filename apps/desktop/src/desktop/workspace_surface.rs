//! Workspace surface.

use super::*;

/// The persisted chart time zone, or the chart default for legacy workspaces without one.
fn restored_chart_time_zone(restored: Option<&WorkspaceChartState>) -> String {
    restored
        .map(|state| state.time_zone.as_str())
        .filter(|time_zone| !time_zone.is_empty())
        .unwrap_or(aeris_chart_integration::DEFAULT_TIME_ZONE)
        .to_string()
}

fn restored_context_and_link_state(
    restored: Option<&WorkspaceChartState>,
) -> (bool, ContextPanelTab, u8, u8) {
    let context_visible = restored.is_some_and(|state| state.context_panel_visible);
    let context_tab = restored
        .and_then(|state| ContextPanelTab::from_persisted(state.context_panel_tab))
        .unwrap_or_default();
    let link_group = restored
        .and_then(|state| u8::try_from(state.chart_link_group).ok())
        .filter(|group| *group <= 4)
        .unwrap_or(0);
    let link_flags = restored
        .and_then(|state| u8::try_from(state.chart_link_flags).ok())
        .map_or(0b11, |flags| flags & 0b11);
    (context_visible, context_tab, link_group, link_flags)
}

pub(super) fn restored_context_panel_height(restored: Option<&WorkspaceChartState>) -> f32 {
    restored
        .map(|state| state.context_panel_height)
        .filter(|height| *height != 0)
        .and_then(|height| height.to_f32())
        .map_or(CONTEXT_PANEL_INITIAL_HEIGHT, clamped_context_panel_height)
}

pub(super) fn restored_chart_trading_visibility(
    restored: Option<&WorkspaceChartState>,
) -> ChartTradingVisibilitySettings {
    ChartTradingVisibilitySettings {
        show_order_management_lines: restored
            .and_then(|state| state.show_order_management_lines)
            .unwrap_or(true),
        show_execution_marks: restored
            .and_then(|state| state.show_execution_marks)
            .unwrap_or(true),
    }
}

struct WorkspaceSurfaceInitialization {
    startup: TerminalStartupState,
    market_worker: MarketDataWorker,
    lifecycle: DesktopLifecycle,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    timeframe_input: Entity<InputState>,
    time_zone_input: Entity<InputState>,
    chart_chrome: chart_chrome::ChartChromePreferences,
    restored_chart_state: Option<WorkspaceChartState>,
    restored_rithmic: Option<(ChartInterval, Option<(i64, i64)>)>,
    interval: ChartInterval,
    symbol_browser: rithmic_shell::RithmicSymbolBrowser,
    studies: RuntimeStudiesState,
    price_alerts: Vec<WorkspacePriceAlertState>,
    price_alert_message: Option<String>,
    bridge_label: String,
    order_book: Entity<ReadOnlyOrderBookView>,
    context_view: Option<ContextView>,
    context_snapshot: Arc<ContextSnapshot>,
    context_panel_visible: bool,
    context_panel_tab: ContextPanelTab,
    context_panel_height: f32,
    chart_link_group: u8,
    chart_link_flags: u8,
    chart_time_zone: String,
    chart_trading_visibility: ChartTradingVisibilitySettings,
    chrome_focus: FocusHandle,
    theme: AerisTheme,
}

fn workspace_surface_from_initialization(init: WorkspaceSurfaceInitialization) -> WorkspaceSurface {
    WorkspaceSurface {
        chart: init.startup.chart,
        order_book: init.order_book,
        trade_tape: None,
        trade_sweeps: Arc::from([]),
        time_sales_filter: TimeSalesFilter::default(),
        context_view: init.context_view,
        context_snapshot: init.context_snapshot,
        context_panel_visible: init.context_panel_visible,
        context_panel_tab: init.context_panel_tab,
        context_panel_height: init.context_panel_height,
        context_credential_dialog: None,
        context_credential_message: None,
        economic_event_risk_message: None,
        chart_link_group: init.chart_link_group,
        chart_link_flags: init.chart_link_flags,
        chart_time_zone: init.chart_time_zone,
        pending_chart_sync_events: VecDeque::new(),
        pending_linked_instrument: None,
        side_panels: SidePanelVisibility::default(),
        side_panel_width: SIDE_PANEL_INITIAL_WIDTH,
        side_panel_split_basis_points: 5_000,
        menu_state: WorkspaceMenuState::default(),
        scrolls: WorkspaceScrollHandles::default(),
        chart_state: init.startup.chart_state,
        chart_state_message: init.startup.chart_state_message,
        theme: init.theme,
        replay_label: init.startup.replay_label,
        worker_label: init.startup.worker_label,
        subscription_id: init.startup.subscription_id,
        bridge_label: init.bridge_label,
        market_worker: init.market_worker,
        lifecycle: init.lifecycle,
        pending_ui_diagnostics: None,
        connection_state: init.startup.connection_state,
        market_session_status: None,
        connection_message: init.startup.connection_message,
        provider_transport_rtt_nanos: None,
        trading_pnl: TradingPnlState::default(),
        chart_trading_visibility: init.chart_trading_visibility,
        symbol_browser: init.symbol_browser,
        symbol_message: initial_symbol_message(init.startup.provider),
        market_state: WorkspaceMarketState::default(),
        symbol_selection_target: SymbolSelectionTarget::Chart,
        pending_symbol_selection_target: None,
        pending_watchlist_instrument: None,
        pending_mnemonic_symbol: None,
        series_message: "Select a symbol before choosing a series".to_string(),
        symbol_input: init.symbol_input,
        indicator_input: init.indicator_input,
        timeframe_input: init.timeframe_input,
        time_zone_input: init.time_zone_input,
        indicator_message: None,
        studies: init.studies,
        study_settings_dialog: None,
        big_trades_dialog: None,
        chrome_overlay: None,
        chrome_overlay_phase: ChromeOverlayPhase::Opening,
        chrome_overlay_generation: 0,
        chrome_overlay_trigger_position: None,
        timeframe_menu_flyout: None,
        timeframe_flyout_close_token: 0,
        timeframe_hover_regions: 0,
        timeframe_trigger_bounds: None,
        chart_type_trigger_bounds: None,
        time_zone_trigger_bounds: None,
        chrome_selection: 0,
        chrome_focus: init.chrome_focus,
        provider: init.startup.provider,
        symbol_provider: init.startup.provider,
        product: init.startup.product,
        rithmic_switch: RithmicSwitchState::Idle,
        interval: init.interval,
        rithmic_pending_interval: None,
        rithmic_pending_product: None,
        rithmic_pending_sequence: None,
        rithmic_previous_selection: None,
        restored_viewport: init.restored_rithmic.and_then(|restored| restored.1),
        last_persisted_viewport: None,
        last_chart_clock_revision: 0,
        pending_chart_context_menu: None,
        pending_pane_activate: PaneActivationRequest::None,
        pending_settings_request: None,
        pending_study_remove_request: None,
        resource_class: ConsumerResourceClass::Foreground,
        chart_chrome: init.chart_chrome,
        retained_chart_presentation: RetainedChartPresentation::default(),
        restored_chart_state: init.restored_chart_state,
        chart_persistence_dirty: false,
        last_chart_user_state_revision: 0,
        price_alerts: init.price_alerts,
        price_alert_dialog: None,
        price_alert_message: init.price_alert_message,
        #[cfg(feature = "diagnostics")]
        foreground_interactions: ForegroundInteractionDiagnostics::default(),
        #[cfg(feature = "diagnostics")]
        live_evidence_enabled: std::env::var_os("AERIS_LIVE_EVIDENCE").is_some(),
        #[cfg(feature = "diagnostics")]
        live_evidence_publications: 0,
    }
}

fn restore_price_alerts(
    restored_chart_state: Option<&WorkspaceChartState>,
    market_worker: &MarketDataWorker,
) -> (Vec<WorkspacePriceAlertState>, Option<String>) {
    let alerts = restored_chart_state
        .map(|state| state.price_alerts.clone())
        .unwrap_or_default();
    let message = (!alerts.is_empty())
        .then(|| {
            market_worker
                .try_replace_price_alerts(runtime_price_alerts(&alerts))
                .err()
        })
        .flatten()
        .map(|_| "Price alerts could not be queued for live monitoring".to_string());
    (alerts, message)
}

fn chart_bridge_label(chart: Option<&Entity<AerisChartView>>, cx: &App) -> String {
    chart.map_or_else(
        || "bridge awaiting snapshot".to_string(),
        |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
    )
}

/// Only a provider calendar that positively reports the product's session as closed stops the
/// bar countdown; an unknown or missing calendar (e.g. providers without one) keeps it running.
fn chart_market_trading(
    status: Option<&aeris_contracts::MarketSessionStatus>,
    product: Option<&InstallProviderInstrument>,
) -> bool {
    !status.is_some_and(|status| {
        status.phase == aeris_contracts::MarketSessionPhase::Closed
            && product.is_some_and(|product| product.instrument_id == status.instrument_id)
    })
}

fn order_flow_aggregation(interval: ChartInterval) -> OrderFlowAggregation {
    match interval.aggregation() {
        ChartAggregation::Trades(count) => OrderFlowAggregation::Trades(count.get()),
        ChartAggregation::FixedSeconds(seconds) => {
            OrderFlowAggregation::TimeMicros(u64::from(seconds.get()).saturating_mul(1_000_000))
        }
        // Aeris Charts B3 supports fixed-time, trade-count, and volume
        // footprint bars. The platform month interval therefore uses the
        // existing deterministic 30-day presentation bucket without changing
        // provider history semantics.
        ChartAggregation::CalendarMonth => OrderFlowAggregation::TimeMicros(
            30_u64
                .saturating_mul(24)
                .saturating_mul(60)
                .saturating_mul(60)
                .saturating_mul(1_000_000),
        ),
    }
}

fn scaled_market_value(value: i64, scale: u8) -> Option<f64> {
    let divisor = 10_f64.powi(i32::from(scale));
    let converted = num_traits::ToPrimitive::to_f64(&value)? / divisor;
    converted.is_finite().then_some(converted)
}

fn chart_order_flow_trades(
    snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot,
) -> Option<Vec<OrderFlowTrade>> {
    snapshot
        .trades
        .iter()
        .map(|retained| {
            let trade = retained.trade.as_ref();
            let timestamp_nanos = trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .or(trade.metadata.timestamps.provider_unix_nanos)
                .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
            Some(OrderFlowTrade {
                ingestion_ordinal: retained.ingestion_ordinal,
                timestamp_micros: timestamp_nanos.div_euclid(1_000),
                price: scaled_market_value(trade.price, snapshot.price_scale)?,
                volume: scaled_market_value(trade.quantity, snapshot.quantity_scale)?,
                aggressor: match trade.aggressor {
                    aeris_market_data::AggressorSide::Buy => ChartAggressorSide::Buy,
                    aeris_market_data::AggressorSide::Sell => ChartAggressorSide::Sell,
                    aeris_market_data::AggressorSide::Unknown => ChartAggressorSide::Unknown,
                },
                session_id: snapshot.provider_generation,
            })
        })
        .collect()
}

fn initial_symbol_browser(startup: &MarketWorkerStartup) -> rithmic_shell::RithmicSymbolBrowser {
    match startup {
        MarketWorkerStartup::Rithmic => rithmic_shell::RithmicSymbolBrowser::default(),
        MarketWorkerStartup::Loading(_) => demand_startup_symbol_browser(),
    }
}

/// Market-demand endpoints issue their own startup catalog search as request 1.
fn demand_startup_symbol_browser() -> rithmic_shell::RithmicSymbolBrowser {
    rithmic_shell::RithmicSymbolBrowser::rithmic_catalog_awaiting_search(
        std::num::NonZeroUsize::MIN,
        "",
    )
}

fn restored_market_selection(
    startup: &MarketWorkerStartup,
) -> Option<(ChartInterval, Option<(i64, i64)>)> {
    match startup {
        MarketWorkerStartup::Loading(startup) => {
            Some((startup.interval, startup.restored_viewport))
        }
        MarketWorkerStartup::Rithmic => None,
    }
}

pub(super) fn persisted_chart_appearance(
    appearance: &ChartAppearanceSettings,
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
        line_color: appearance.line_color.clone(),
        line_width: u32::from(appearance.line_width),
        line_style: u32::from(appearance.line_style),
        area_top_color: appearance.area_top_color.clone(),
        baseline_top_color: appearance.baseline_top_color.clone(),
        baseline_bottom_color: appearance.baseline_bottom_color.clone(),
    }
}

pub(super) fn restored_chart_appearance(
    appearance: &WorkspaceChartAppearanceState,
) -> Option<ChartAppearanceSettings> {
    let grid_style = u8::try_from(appearance.grid_style).ok()?.min(4);
    let crosshair_width = u8::try_from(appearance.crosshair_width).ok()?.clamp(1, 4);
    let crosshair_style = u8::try_from(appearance.crosshair_style).ok()?.min(4);
    let line_width = u8::try_from(appearance.line_width).ok()?.clamp(1, 4);
    let line_style = u8::try_from(appearance.line_style).ok()?.min(4);
    Some(ChartAppearanceSettings {
        grid_visible: appearance.grid_visible,
        grid_color: restored_appearance_color(&appearance.grid_color, |colors| colors.grid),
        grid_style,
        crosshair_color: restored_appearance_color(&appearance.crosshair_color, |colors| {
            colors.crosshair
        }),
        crosshair_width,
        crosshair_style,
        up_color: restored_appearance_color(&appearance.up_color, |colors| colors.bullish),
        down_color: restored_appearance_color(&appearance.down_color, |colors| colors.bearish),
        wick_up_color: restored_appearance_color(&appearance.wick_up_color, |colors| {
            colors.bullish
        }),
        wick_down_color: restored_appearance_color(&appearance.wick_down_color, |colors| {
            colors.bearish
        }),
        border_up_color: restored_appearance_color(&appearance.border_up_color, |colors| {
            colors.bullish
        }),
        border_down_color: restored_appearance_color(&appearance.border_down_color, |colors| {
            colors.bearish
        }),
        wick_visible: appearance.wick_visible,
        border_visible: appearance.border_visible,
        open_visible: appearance.open_visible,
        thin_bars: appearance.thin_bars,
        line_color: appearance.line_color.clone(),
        line_width,
        line_style,
        area_top_color: appearance.area_top_color.clone(),
        baseline_top_color: appearance.baseline_top_color.clone(),
        baseline_bottom_color: appearance.baseline_bottom_color.clone(),
    })
}

/// Durable marker for an appearance color that follows the active chart theme.
const THEME_APPEARANCE_COLOR: &str = "theme";

fn persisted_appearance_color(color: &ChartAppearanceColor) -> String {
    match color {
        ChartAppearanceColor::Theme => THEME_APPEARANCE_COLOR.to_string(),
        ChartAppearanceColor::Custom(color) => color.clone(),
    }
}

/// Layouts written before typed provenance flattened a theme-following color into that
/// theme's concrete default, so either theme's default for the role follows the theme again.
fn restored_appearance_color(
    color: &str,
    role: fn(ChartThemeColors) -> &'static str,
) -> ChartAppearanceColor {
    let theme_default = [AerisChartTheme::Light, AerisChartTheme::Dark]
        .into_iter()
        .any(|theme| role(ChartThemeColors::for_theme(theme)).eq_ignore_ascii_case(color));
    if color == THEME_APPEARANCE_COLOR || theme_default {
        ChartAppearanceColor::Theme
    } else {
        ChartAppearanceColor::Custom(color.to_string())
    }
}

/// Tape-derived order-flow indicators the indicator menu can add back after removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrderFlowMenuStudy {
    CumulativeDelta,
    Delta,
    BigTrades,
}

impl OrderFlowMenuStudy {
    pub(super) const ALL: [Self; 3] = [Self::CumulativeDelta, Self::Delta, Self::BigTrades];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::CumulativeDelta => "Cumulative delta (CVD)",
            Self::Delta => "Delta histogram",
            Self::BigTrades => "Big trades",
        }
    }

    pub(super) const fn is_shown(self, settings: OrderFlowSettings) -> bool {
        match self {
            Self::CumulativeDelta => settings.show_cumulative_delta,
            Self::Delta => settings.show_delta_histogram,
            Self::BigTrades => settings.big_trades.is_some(),
        }
    }
}

fn persisted_big_trades(settings: BigTradesSettings) -> WorkspaceBigTradesState {
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

fn restored_big_trades(state: &WorkspaceBigTradesState) -> Option<BigTradesSettings> {
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
const fn big_trades_filter_draft(
    filter: BigTradesFilter,
) -> (Option<BigTradesIntensity>, Option<f64>) {
    match filter {
        BigTradesFilter::Auto { intensity } => (Some(intensity), None),
        BigTradesFilter::Fixed { minimum_volume } => (None, Some(minimum_volume)),
    }
}

fn parse_big_trades_minimum_volume(text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|volume| volume.is_finite() && *volume > 0.0)
        .ok_or_else(|| "Minimum volume must be a number greater than zero".to_string())
}

/// Editable text for an order volume. Sums of fractional prints carry binary rounding noise, so
/// the text is rounded unless that would erase a tiny but valid volume.
fn big_trades_volume_text(volume: f64) -> String {
    let rounded = format!("{volume:.6}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed.parse::<f64>().is_ok_and(|value| value > 0.0) {
        trimmed.to_string()
    } else {
        volume.to_string()
    }
}

fn persisted_order_flow_settings(settings: OrderFlowSettings) -> WorkspaceOrderFlowSettingsState {
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
    }
}

fn restored_order_flow_settings(
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
    Some(OrderFlowSettings {
        display_mode,
        show_cumulative_delta: settings.study_visibility_revision >= 1
            && settings.show_cumulative_delta,
        show_delta_histogram: settings.study_visibility_revision >= 1
            && settings.show_delta_histogram,
        big_trades,
        ticks_per_row: settings.ticks_per_row,
    })
}

const fn chart_study_plot(plot: StudyPlotKind) -> ChartStudyPlotKind {
    match plot {
        StudyPlotKind::Line => ChartStudyPlotKind::Line,
        StudyPlotKind::Histogram => ChartStudyPlotKind::Histogram,
        StudyPlotKind::Area => ChartStudyPlotKind::Area,
    }
}

const fn chart_study_pane(pane: StudyPaneTarget) -> ChartStudyPaneTarget {
    match pane {
        StudyPaneTarget::Price => ChartStudyPaneTarget::Price,
        StudyPaneTarget::Dedicated { group } => ChartStudyPaneTarget::Dedicated { group },
    }
}

const fn chart_study_scale(scale: StudyScaleTarget) -> ChartStudyScaleTarget {
    match scale {
        StudyScaleTarget::Primary => ChartStudyScaleTarget::Primary,
        StudyScaleTarget::Left => ChartStudyScaleTarget::Left,
        StudyScaleTarget::Overlay => ChartStudyScaleTarget::Overlay,
    }
}

const fn chart_study_point_style(style: StudyPointStyle) -> ChartStudyPointStyle {
    match style {
        StudyPointStyle::Uniform => ChartStudyPointStyle::Uniform,
        StudyPointStyle::MomentumHistogram => ChartStudyPointStyle::MomentumHistogram,
    }
}

const fn chart_study_input_requirements(
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

fn chart_study_threshold(
    region: Option<StudyThresholdRegion>,
) -> Option<ChartStudyThresholdRegion> {
    region.and_then(|region| {
        Some(ChartStudyThresholdRegion {
            lower: region.lower.mantissa.to_f64()? / 10_f64.powi(i32::from(region.lower.scale)),
            upper: region.upper.mantissa.to_f64()? / 10_f64.powi(i32::from(region.upper.scale)),
        })
    })
}

const fn runtime_managed_indicator(indicator: ChartIndicator) -> bool {
    matches!(
        indicator,
        ChartIndicator::Sma
            | ChartIndicator::Ema
            | ChartIndicator::EmaRibbon
            | ChartIndicator::Wma
            | ChartIndicator::Bollinger
            | ChartIndicator::Vwap
            | ChartIndicator::Rsi
            | ChartIndicator::Macd
            | ChartIndicator::Stochastic
            | ChartIndicator::Atr
    )
}

fn persisted_legacy_indicator_states(
    states: impl IntoIterator<Item = ChartIndicatorState>,
) -> Vec<WorkspaceChartIndicatorState> {
    states
        .into_iter()
        .filter(|state| !runtime_managed_indicator(state.indicator))
        .map(|state| WorkspaceChartIndicatorState {
            kind: state.indicator.identifier().to_string(),
            visible: state.visible,
        })
        .collect()
}

fn runtime_study_count(studies: &RuntimeStudiesState) -> usize {
    studies
        .active
        .iter()
        .filter(|state| {
            !studies.removing.contains(&state.study_id)
                && !studies.automatic_removals.contains(&state.study_id)
        })
        .count()
        + studies
            .pending
            .values()
            .filter(|state| !state.remove_on_registration)
            .count()
        + studies.deferred.len()
}

fn persisted_runtime_study_states(
    studies: &RuntimeStudiesState,
    mut visible: impl FnMut(StudyInstanceId) -> Option<bool>,
) -> Vec<WorkspaceChartStudyState> {
    let mut persisted = studies
        .active
        .iter()
        .filter(|state| !studies.automatic_removals.contains(&state.study_id))
        .map(|state| {
            let mut persisted = state.persisted.clone();
            persisted.visible = visible(state.study_id).unwrap_or(persisted.visible);
            persisted
        })
        .chain(
            studies
                .pending
                .values()
                .filter(|state| !state.remove_on_registration)
                .map(|state| state.persisted.clone()),
        )
        .chain(
            studies
                .deferred
                .iter()
                .filter(|state| !state.remove_on_registration)
                .map(|state| state.persisted.clone()),
        )
        .collect::<Vec<_>>();
    persisted.sort_by_key(|state| state.local_id);
    persisted
}

fn dispatch_automatic_study_removals(
    studies: &mut RuntimeStudiesState,
    mut try_remove: impl FnMut(StudyInstanceId) -> Result<(), TrySendError<StudyInstanceId>>,
) -> bool {
    let mut retry = studies
        .automatic_removals
        .iter()
        .filter(|study_id| !studies.removing.contains(study_id))
        .copied()
        .collect::<Vec<_>>();
    retry.sort_unstable();
    for study_id in retry {
        match try_remove(study_id) {
            Ok(()) => {
                studies.removing.insert(study_id);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return false,
        }
    }
    true
}

fn study_removal_failed(studies: &mut RuntimeStudiesState, study_id: StudyInstanceId) -> bool {
    studies.removing.remove(&study_id);
    studies.automatic_removals.contains(&study_id)
}

fn discard_unregistered_runtime_studies(studies: &mut RuntimeStudiesState) -> bool {
    let changed = !studies.deferred.is_empty()
        || studies
            .pending
            .values()
            .any(|state| !state.remove_on_registration);
    for pending in studies.pending.values_mut() {
        pending.remove_on_registration = true;
    }
    studies.deferred.clear();
    changed
}

fn next_deferred_runtime_study_index(studies: &RuntimeStudiesState) -> Option<usize> {
    studies.deferred.iter().position(|state| !state.blocked)
}

fn runtime_study_registration(
    state: &WorkspaceChartStudyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
) -> Result<NativeStudyRegistration, String> {
    let registry = super::study_packages::product_study_registry()
        .map_err(|error| format!("trusted study package registry is invalid: {error}"))?;
    runtime_study_registration_with_registry(state, current_series, active, &registry)
}

fn runtime_study_registration_with_registry(
    state: &WorkspaceChartStudyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
    registry: &aeris_study_sdk::TrustedStudyRegistry<'_>,
) -> Result<NativeStudyRegistration, String> {
    let dependencies = state
        .dependencies
        .iter()
        .map(|dependency| runtime_study_dependency(dependency, current_series, active))
        .collect::<Result<Vec<_>, _>>()?;
    let settings = state
        .settings
        .iter()
        .map(|setting| {
            let value = setting
                .value
                .as_ref()
                .ok_or_else(|| format!("study setting {} has no value", setting.identifier))?;
            Ok((
                setting.identifier.clone(),
                runtime_study_setting_value(value)?,
            ))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
    let registration = registry
        .restore(
            &state.identifier,
            state.implementation_revision,
            dependencies,
            settings,
        )
        .map_err(|error| error.to_string())?;
    let output_identifiers = registration
        .definition
        .outputs
        .iter()
        .map(|output| output.identifier.as_str())
        .collect::<Vec<_>>();
    if output_identifiers
        != state
            .output_identifiers
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    {
        return Err(format!(
            "study {} output interface no longer matches this workspace",
            state.identifier
        ));
    }
    Ok(registration)
}

fn runtime_study_setting_value(
    value: &workspace_study_setting_state::Value,
) -> Result<StudySettingValue, String> {
    Ok(match value {
        workspace_study_setting_state::Value::Boolean(value) => StudySettingValue::Boolean(*value),
        workspace_study_setting_state::Value::Integer(value) => StudySettingValue::Integer(*value),
        workspace_study_setting_state::Value::Decimal(value) => {
            StudySettingValue::Decimal(StudyDecimal {
                mantissa: value.mantissa,
                scale: u8::try_from(value.scale)
                    .map_err(|_| "study decimal scale is out of range".to_string())?,
            })
        }
        workspace_study_setting_state::Value::Text(value) => StudySettingValue::Text(value.clone()),
        workspace_study_setting_state::Value::Choice(value) => {
            StudySettingValue::Choice(value.clone())
        }
    })
}

fn persisted_study_setting_value(
    value: &StudySettingValue,
) -> workspace_study_setting_state::Value {
    match value {
        StudySettingValue::Boolean(value) => workspace_study_setting_state::Value::Boolean(*value),
        StudySettingValue::Integer(value) => workspace_study_setting_state::Value::Integer(*value),
        StudySettingValue::Decimal(value) => {
            workspace_study_setting_state::Value::Decimal(WorkspaceStudyDecimalState {
                mantissa: value.mantissa,
                scale: u32::from(value.scale),
            })
        }
        StudySettingValue::Text(value) => workspace_study_setting_state::Value::Text(value.clone()),
        StudySettingValue::Choice(value) => {
            workspace_study_setting_state::Value::Choice(value.clone())
        }
    }
}

pub(super) fn study_decimal_text(value: StudyDecimal) -> String {
    let scale = usize::from(value.scale);
    if scale == 0 {
        return value.mantissa.to_string();
    }
    let negative = value.mantissa < 0;
    let magnitude = i128::from(value.mantissa).abs();
    let factor = 10_i128.pow(u32::from(value.scale));
    let whole = magnitude / factor;
    let fraction = magnitude % factor;
    format!(
        "{}{whole}.{fraction:0width$}",
        if negative { "-" } else { "" },
        width = scale
    )
}

fn parse_study_decimal(text: &str) -> Result<StudyDecimal, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("decimal setting cannot be empty".to_string());
    }
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text), |value| (true, value));
    let unsigned = unsigned.strip_prefix('+').unwrap_or(unsigned);
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len()
            > usize::from(aeris_market_runtime::study::MAXIMUM_STUDY_SETTING_DECIMAL_SCALE)
    {
        return Err("decimal setting is invalid".to_string());
    }
    let whole = if whole.is_empty() { "0" } else { whole };
    let scale = u8::try_from(fraction.len()).map_err(|_| "decimal scale is invalid".to_string())?;
    let factor = 10_i128.pow(u32::from(scale));
    let whole = whole
        .parse::<i128>()
        .map_err(|_| "decimal setting is out of range".to_string())?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| "decimal setting is out of range".to_string())?
    };
    let magnitude = whole
        .checked_mul(factor)
        .and_then(|value| value.checked_add(fraction))
        .ok_or_else(|| "decimal setting is out of range".to_string())?;
    let signed = if negative { -magnitude } else { magnitude };
    let mantissa =
        i64::try_from(signed).map_err(|_| "decimal setting is out of range".to_string())?;
    Ok(StudyDecimal { mantissa, scale })
}

fn study_setting_input_text(value: &StudySettingValue) -> Option<String> {
    match value {
        StudySettingValue::Integer(value) => Some(value.to_string()),
        StudySettingValue::Decimal(value) => Some(study_decimal_text(*value)),
        StudySettingValue::Text(value) => Some(value.clone()),
        StudySettingValue::Boolean(_) | StudySettingValue::Choice(_) => None,
    }
}

fn study_setting_value_from_dialog(
    dialog: &StudySettingsDialogState,
    spec: &StudySettingSpec,
    cx: &App,
) -> Result<StudySettingValue, String> {
    match &spec.presentation.control {
        StudySettingControl::Boolean | StudySettingControl::Choice { .. } => dialog
            .draft_values
            .get(&spec.identifier)
            .cloned()
            .ok_or_else(|| format!("{} has no value", spec.presentation.label)),
        StudySettingControl::Integer { .. } => {
            let input = dialog
                .inputs
                .get(&spec.identifier)
                .ok_or_else(|| format!("{} editor is unavailable", spec.presentation.label))?;
            input
                .read(cx)
                .value()
                .trim()
                .parse::<i64>()
                .map(StudySettingValue::Integer)
                .map_err(|_| format!("{} must be a whole number", spec.presentation.label))
        }
        StudySettingControl::Decimal { .. } => {
            let input = dialog
                .inputs
                .get(&spec.identifier)
                .ok_or_else(|| format!("{} editor is unavailable", spec.presentation.label))?;
            parse_study_decimal(input.read(cx).value().as_ref())
                .map(StudySettingValue::Decimal)
                .map_err(|error| format!("{}: {error}", spec.presentation.label))
        }
        StudySettingControl::Text => {
            let input = dialog
                .inputs
                .get(&spec.identifier)
                .ok_or_else(|| format!("{} editor is unavailable", spec.presentation.label))?;
            Ok(StudySettingValue::Text(input.read(cx).value().to_string()))
        }
    }
}

pub(super) fn study_setting_condition_matches(
    dialog: &StudySettingsDialogState,
    condition: Option<&StudySettingCondition>,
    cx: &App,
) -> bool {
    let Some(condition) = condition else {
        return true;
    };
    let Some(spec) = dialog
        .specs
        .iter()
        .find(|spec| spec.identifier == condition.setting_identifier)
    else {
        return false;
    };
    study_setting_value_from_dialog(dialog, spec, cx).is_ok_and(|value| value == condition.equals)
}

pub(super) fn study_display_name(identifier: &str) -> String {
    match identifier {
        aeris_study_sdk::BUILTIN_SMA_IDENTIFIER => "Simple Moving Average".to_string(),
        aeris_study_sdk::BUILTIN_EMA_IDENTIFIER => "Exponential Moving Average".to_string(),
        aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER => "EMA Ribbon".to_string(),
        aeris_study_sdk::BUILTIN_WMA_IDENTIFIER => "Weighted Moving Average".to_string(),
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER => "Bollinger Bands".to_string(),
        aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER => "Volume Weighted Average Price".to_string(),
        aeris_study_sdk::BUILTIN_RSI_IDENTIFIER => "Relative Strength Index".to_string(),
        aeris_study_sdk::BUILTIN_MACD_IDENTIFIER => "MACD".to_string(),
        aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER => "Stochastic".to_string(),
        aeris_study_sdk::BUILTIN_ATR_IDENTIFIER => "Average True Range".to_string(),
        _ => identifier.to_string(),
    }
}

fn runtime_study_streams(streams: &[i32]) -> Result<StreamRequirements, String> {
    let mut requirements = StreamRequirements::NONE;
    for stream in streams {
        let stream = WorkspaceStudyMarketStream::try_from(*stream)
            .map_err(|_| "study market dependency contains an unknown stream".to_string())?;
        let stream = match stream {
            WorkspaceStudyMarketStream::Bars => MarketStream::Bars,
            WorkspaceStudyMarketStream::Trades => MarketStream::Trades,
            WorkspaceStudyMarketStream::Quotes => MarketStream::Quotes,
            WorkspaceStudyMarketStream::Depth => MarketStream::Depth,
            WorkspaceStudyMarketStream::Unspecified => {
                return Err("study market dependency contains an unspecified stream".to_string());
            }
        };
        requirements = requirements.with(stream);
    }
    if requirements.is_empty() {
        return Err("study market dependency has no streams".to_string());
    }
    Ok(requirements)
}

fn runtime_study_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    let cadence = SeriesCadence::try_from(series.cadence)
        .map_err(|_| "study series cadence is unknown".to_string())?;
    let period = match cadence {
        SeriesCadence::FixedSeconds => aeris_market_data::BarPeriod::time(series.cadence_value),
        SeriesCadence::Trades => aeris_market_data::BarPeriod::tick(series.cadence_value),
        SeriesCadence::SessionDays => aeris_market_data::BarPeriod::session(series.cadence_value),
        SeriesCadence::CalendarWeeks => aeris_market_data::BarPeriod::week(series.cadence_value),
        SeriesCadence::CalendarMonths => aeris_market_data::BarPeriod::month(series.cadence_value),
        SeriesCadence::Unspecified => {
            return Err("study series cadence is unspecified".to_string());
        }
    }
    .map_err(|error| error.to_string())?;
    let series = BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        period,
        definition_version: series.definition_revision,
    };
    series.validate().map_err(|error| error.to_string())?;
    Ok(series)
}

fn runtime_study_dependency(
    dependency: &WorkspaceStudyDependencyState,
    current_series: &BarSeriesKey,
    active: &[RuntimeStudyState],
) -> Result<StudyDependency, String> {
    match WorkspaceStudyDependencyKind::try_from(dependency.kind)
        .map_err(|_| "study dependency kind is unknown".to_string())?
    {
        WorkspaceStudyDependencyKind::CurrentChartSeries => {
            Ok(StudyDependency::Market(StudyMarketInput {
                series: current_series.clone(),
                streams: runtime_study_streams(&dependency.streams)?,
            }))
        }
        WorkspaceStudyDependencyKind::ExplicitSeries => {
            let series = dependency
                .series
                .as_ref()
                .ok_or_else(|| "study explicit-series dependency has no series".to_string())?;
            Ok(StudyDependency::Market(StudyMarketInput {
                series: runtime_study_series(series)?,
                streams: runtime_study_streams(&dependency.streams)?,
            }))
        }
        WorkspaceStudyDependencyKind::StudyOutput => {
            let upstream = active
                .iter()
                .find(|state| state.persisted.local_id == dependency.study_local_id)
                .ok_or_else(|| {
                    format!(
                        "study dependency {} has not been restored yet",
                        dependency.study_local_id
                    )
                })?;
            let output_index = upstream
                .persisted
                .output_identifiers
                .iter()
                .position(|identifier| identifier == &dependency.output_identifier)
                .ok_or_else(|| {
                    format!(
                        "study dependency output {} is unavailable",
                        dependency.output_identifier
                    )
                })?;
            Ok(StudyDependency::Output(
                upstream.study_id.output(output_index),
            ))
        }
        WorkspaceStudyDependencyKind::Unspecified => {
            Err("study dependency kind is unspecified".to_string())
        }
    }
}

fn runtime_study_uses_current_chart(state: &WorkspaceChartStudyState) -> bool {
    state.dependencies.iter().any(|dependency| {
        WorkspaceStudyDependencyKind::try_from(dependency.kind)
            .is_ok_and(|kind| kind == WorkspaceStudyDependencyKind::CurrentChartSeries)
    })
}

fn legacy_integer_setting(identifier: &str, value: i64) -> WorkspaceStudySettingState {
    WorkspaceStudySettingState {
        identifier: identifier.to_string(),
        value: Some(workspace_study_setting_state::Value::Integer(value)),
    }
}

fn legacy_runtime_study_contract(
    indicator: ChartIndicator,
) -> Option<(
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
)> {
    let contract = match indicator {
        ChartIndicator::Sma => (
            aeris_study_sdk::BUILTIN_SMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_SMA_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_SMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Ema => (
            aeris_study_sdk::BUILTIN_EMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_EMA_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_EMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_EMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_EMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::EmaRibbon => legacy_ema_ribbon_contract(),
        ChartIndicator::Wma => (
            aeris_study_sdk::BUILTIN_WMA_IDENTIFIER,
            aeris_study_sdk::BUILTIN_WMA_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_WMA_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_WMA_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_WMA_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Bollinger => legacy_bollinger_contract(),
        ChartIndicator::Vwap => (
            aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER,
            aeris_study_sdk::BUILTIN_VWAP_IMPLEMENTATION_REVISION,
            Vec::new(),
            vec![aeris_study_sdk::BUILTIN_VWAP_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Rsi => (
            aeris_study_sdk::BUILTIN_RSI_IDENTIFIER,
            aeris_study_sdk::BUILTIN_RSI_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_RSI_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_RSI_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_RSI_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Macd => legacy_macd_contract(),
        ChartIndicator::Stochastic => legacy_stochastic_contract(),
        ChartIndicator::Atr => (
            aeris_study_sdk::BUILTIN_ATR_IDENTIFIER,
            aeris_study_sdk::BUILTIN_ATR_IMPLEMENTATION_REVISION,
            vec![legacy_integer_setting(
                aeris_study_sdk::BUILTIN_ATR_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_ATR_DEFAULT_PERIOD,
            )],
            vec![aeris_study_sdk::BUILTIN_ATR_OUTPUT_IDENTIFIER.to_string()],
        ),
        ChartIndicator::Volume | ChartIndicator::VolumeProfile => return None,
    };
    Some(contract)
}

fn legacy_ema_ribbon_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER,
        aeris_study_sdk::BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION,
        aeris_study_sdk::BUILTIN_EMA_RIBBON_PERIOD_SETTINGS
            .iter()
            .zip(aeris_study_sdk::BUILTIN_EMA_RIBBON_DEFAULT_PERIODS)
            .map(|(identifier, period)| legacy_integer_setting(identifier, period))
            .collect(),
        aeris_study_sdk::BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS
            .iter()
            .map(|identifier| (*identifier).to_string())
            .collect(),
    )
}

fn legacy_macd_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_MACD_IDENTIFIER,
        aeris_study_sdk::BUILTIN_MACD_IMPLEMENTATION_REVISION,
        vec![
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_MACD_FAST_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_FAST_PERIOD,
            ),
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_MACD_SLOW_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_SLOW_PERIOD,
            ),
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_MACD_SIGNAL_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_MACD_DEFAULT_SIGNAL_PERIOD,
            ),
        ],
        vec![
            aeris_study_sdk::BUILTIN_MACD_LINE_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_MACD_SIGNAL_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_MACD_HISTOGRAM_OUTPUT_IDENTIFIER.to_string(),
        ],
    )
}

fn legacy_stochastic_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER,
        aeris_study_sdk::BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION,
        vec![
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_STOCHASTIC_K_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD,
            ),
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_STOCHASTIC_D_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_STOCHASTIC_DEFAULT_D_PERIOD,
            ),
        ],
        vec![
            aeris_study_sdk::BUILTIN_STOCHASTIC_K_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_STOCHASTIC_D_OUTPUT_IDENTIFIER.to_string(),
        ],
    )
}

fn legacy_bollinger_contract() -> (
    &'static str,
    u32,
    Vec<WorkspaceStudySettingState>,
    Vec<String>,
) {
    (
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER,
        aeris_study_sdk::BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION,
        vec![
            legacy_integer_setting(
                aeris_study_sdk::BUILTIN_BOLLINGER_PERIOD_SETTING,
                aeris_study_sdk::BUILTIN_BOLLINGER_DEFAULT_PERIOD,
            ),
            WorkspaceStudySettingState {
                identifier: aeris_study_sdk::BUILTIN_BOLLINGER_DEVIATION_SETTING.to_string(),
                value: Some(workspace_study_setting_state::Value::Decimal(
                    WorkspaceStudyDecimalState {
                        mantissa: aeris_study_sdk::BUILTIN_BOLLINGER_DEFAULT_DEVIATION.mantissa,
                        scale: u32::from(
                            aeris_study_sdk::BUILTIN_BOLLINGER_DEFAULT_DEVIATION.scale,
                        ),
                    },
                )),
            },
        ],
        vec![
            aeris_study_sdk::BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
        ],
    )
}

/// Stroke width a persisted study draws with. Zero means the user never chose one (or the
/// state predates the setting), so the study's default applies.
fn persisted_study_line_width(persisted: &WorkspaceChartStudyState) -> u8 {
    u8::try_from(persisted.line_width)
        .ok()
        .filter(|width| (1..=MAXIMUM_STUDY_LINE_WIDTH).contains(width))
        .unwrap_or_else(|| default_study_line_width(&persisted.identifier))
}

/// Stroke width a study draws with until the user picks one. Exponential moving averages are
/// one-pixel lines, matching Aeris Charts' own EMA defaults; other studies keep the chart
/// default. Studies reach the chart as external outputs, so this host default is what applies.
fn default_study_line_width(identifier: &str) -> u8 {
    match identifier {
        aeris_study_sdk::BUILTIN_EMA_IDENTIFIER
        | aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER => 1,
        _ => DEFAULT_STUDY_LINE_WIDTH,
    }
}

fn legacy_runtime_study(
    local_id: u64,
    indicator: ChartIndicator,
    visible: bool,
) -> Option<WorkspaceChartStudyState> {
    let (identifier, implementation_revision, settings, output_identifiers) =
        legacy_runtime_study_contract(indicator)?;
    Some(WorkspaceChartStudyState {
        line_width: 0,
        local_id,
        identifier: identifier.to_string(),
        implementation_revision,
        settings,
        dependencies: vec![WorkspaceStudyDependencyState {
            kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
            streams: vec![WorkspaceStudyMarketStream::Bars as i32],
            ..WorkspaceStudyDependencyState::default()
        }],
        visible,
        output_identifiers,
    })
}

fn persisted_runtime_studies(
    restored_chart_state: Option<&WorkspaceChartState>,
) -> Vec<PendingRuntimeStudyState> {
    let Some(state) = restored_chart_state else {
        return Vec::new();
    };
    let mut persisted = state.studies.clone();
    let mut durable_identifiers = std::collections::BTreeMap::<String, usize>::new();
    for study in &persisted {
        *durable_identifiers
            .entry(study.identifier.clone())
            .or_default() += 1;
    }
    let mut used_local_ids = persisted
        .iter()
        .map(|study| study.local_id)
        .collect::<Vec<_>>();
    for item in &state.indicators {
        let Some(indicator) = ChartIndicator::from_identifier(&item.kind) else {
            continue;
        };
        if !runtime_managed_indicator(indicator) {
            continue;
        }
        let mut local_id = 1_u64;
        while used_local_ids.contains(&local_id) {
            let Some(next) = local_id.checked_add(1) else {
                break;
            };
            local_id = next;
        }
        if used_local_ids.contains(&local_id) {
            continue;
        }
        let Some(legacy) = legacy_runtime_study(local_id, indicator, item.visible) else {
            continue;
        };
        if let Some(remaining) = durable_identifiers.get_mut(&legacy.identifier)
            && *remaining > 0
        {
            *remaining -= 1;
            continue;
        }
        used_local_ids.push(local_id);
        persisted.push(legacy);
    }
    persisted
        .into_iter()
        .map(|persisted| PendingRuntimeStudyState {
            persisted,
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: false,
            blocked: false,
        })
        .collect()
}

fn restored_runtime_studies(
    restored_chart_state: Option<&WorkspaceChartState>,
) -> RuntimeStudiesState {
    let deferred = persisted_runtime_studies(restored_chart_state);
    let next_local_id = deferred
        .iter()
        .map(|state| state.persisted.local_id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap_or(0);
    RuntimeStudiesState {
        deferred,
        next_local_id,
        ..RuntimeStudiesState::default()
    }
}

fn restored_workspace_records(
    restored_chart_state: Option<&WorkspaceChartState>,
    market_worker: &MarketDataWorker,
) -> (
    RuntimeStudiesState,
    Vec<WorkspacePriceAlertState>,
    Option<String>,
) {
    let studies = restored_runtime_studies(restored_chart_state);
    let (price_alerts, price_alert_message) =
        restore_price_alerts(restored_chart_state, market_worker);
    (studies, price_alerts, price_alert_message)
}

fn initialize_restored_chart(
    chart: Option<&Entity<AerisChartView>>,
    chart_chrome: chart_chrome::ChartChromePreferences,
    cx: &mut Context<WorkspaceSurface>,
) {
    initialize_chart_chrome(chart, chart_chrome, cx);
    observe_chart(chart, cx);
}

impl WorkspaceSurface {
    pub(super) fn take_chart_persistence_dirty(&mut self) -> bool {
        std::mem::take(&mut self.chart_persistence_dirty)
    }

    /// State to persist while no chart has data to read from (startup, or a symbol or timeframe
    /// switch still loading). The restored snapshot is consumed by the first chart, so a switch
    /// falls back to the presentation retained from the chart it is replacing; either way the
    /// surface-owned time zone is kept, so a save mid-load never resets the pane.
    fn unloaded_chart_state(&self) -> Option<WorkspaceChartState> {
        self.restored_chart_state
            .clone()
            .or_else(|| self.retained_chart_presentation.chart_state.clone())
            .map(|mut state| {
                state.time_zone.clone_from(&self.chart_time_zone);
                state
            })
    }

    pub(super) fn workspace_chart_state(&self, cx: &App) -> Option<WorkspaceChartState> {
        let Some(chart) = self.chart.as_ref() else {
            return self.unloaded_chart_state();
        };
        let chart = chart.read(cx);
        if !chart.has_market_data() {
            return self.unloaded_chart_state();
        }
        let chart_state_json = match chart.export_semantic_state_json() {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Aeris drawings could not be serialized: {error}");
                return self.unloaded_chart_state();
            }
        };
        let price_axis = chart
            .price_axis_menu_state(0, false)
            .or_else(|| chart.price_axis_menu_state(0, true))
            .map(|state| WorkspacePriceAxisState {
                flags: u32::from(state.flags),
                mode: u32::from(state.mode),
                left: state.left,
                precision: state.precision.map(u32::from),
            });
        let indicators = persisted_legacy_indicator_states(chart.indicator_states());
        let studies = persisted_runtime_study_states(&self.studies, |study_id| {
            chart.study_visible(study_id.get())
        });
        Some(WorkspaceChartState {
            chart_type: chart.chart_type().identifier().to_string(),
            chart_state_json,
            indicators,
            price_axis,
            locked_drawing_ids: chart.locked_drawing_ids(),
            crosshair_mode: u32::from(chart.crosshair_mode()),
            price_alerts: self.price_alerts.clone(),
            studies,
            appearance: Some(persisted_chart_appearance(&chart.appearance_settings())),
            order_flow: Some(persisted_order_flow_settings(chart.order_flow_settings())),
            context_panel_visible: self.context_panel_visible,
            context_panel_tab: self.context_panel_tab.persisted(),
            context_panel_height: self.context_panel_height.round().to_u32().unwrap_or(0),
            chart_link_group: u32::from(self.chart_link_group),
            chart_link_flags: u32::from(self.chart_link_flags),
            time_zone: self.chart_time_zone.clone(),
            show_order_management_lines: Some(
                self.chart_trading_visibility.show_order_management_lines,
            ),
            show_execution_marks: Some(self.chart_trading_visibility.show_execution_marks),
        })
    }

    fn apply_restored_chart_state(
        chart: &Entity<AerisChartView>,
        state: &WorkspaceChartState,
        restore_drawings: bool,
        cx: &mut Context<Self>,
    ) {
        let indicators = state
            .indicators
            .iter()
            .filter_map(|item| {
                ChartIndicator::from_identifier(&item.kind).map(|indicator| ChartIndicatorState {
                    indicator,
                    visible: item.visible,
                })
            })
            .filter(|state| !runtime_managed_indicator(state.indicator))
            .take(ChartIndicator::ALL.len())
            .collect::<Vec<_>>();
        let chart_type = ChartType::from_identifier(&state.chart_type);
        let price_axis = state.price_axis.as_ref().and_then(|axis| {
            Some(PriceAxisMenuState {
                flags: u16::try_from(axis.flags).ok()?,
                mode: u8::try_from(axis.mode).ok()?.min(3),
                left: axis.left,
                precision: axis.precision.and_then(|value| u8::try_from(value).ok()),
            })
        });
        let drawing_json = state.chart_state_json.clone();
        let locked = state.locked_drawing_ids.clone();
        let crosshair_mode = u8::try_from(state.crosshair_mode)
            .ok()
            .filter(|mode| *mode <= 3);
        let appearance = state
            .appearance
            .as_ref()
            .and_then(restored_chart_appearance);
        let order_flow = state
            .order_flow
            .as_ref()
            .and_then(restored_order_flow_settings);
        let time_zone = if state.time_zone.is_empty() {
            aeris_chart_integration::DEFAULT_TIME_ZONE
        } else {
            state.time_zone.as_str()
        };
        chart.update(cx, |chart, _| {
            if let Err(error) = chart.set_time_zone(time_zone) {
                eprintln!("Aeris persisted chart time zone could not be restored: {error}");
            }
            if let Some(order_flow) = order_flow {
                let _ = chart.set_order_flow_settings(order_flow);
            }
            if let Some(chart_type) = chart_type {
                chart.set_chart_type(chart_type);
            }
            if let Some(appearance) = &appearance {
                let _ = chart.set_appearance_settings(appearance);
            }
            if let Err(error) = chart.restore_indicator_states(&indicators) {
                eprintln!("Aeris persisted indicators could not be restored: {error}");
            }
            if restore_drawings
                && !drawing_json.is_empty()
                && let Err(error) = chart.import_semantic_state_json(&drawing_json, &locked)
            {
                eprintln!("Aeris persisted drawings could not be restored: {error}");
            }
            if let Some(price_axis) = price_axis {
                let _ = chart.restore_price_axis_menu_state(price_axis);
            }
            if let Some(mode) = crosshair_mode {
                let _ = chart.set_crosshair_mode(mode);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        lifecycle: DesktopLifecycle,
        symbol_input: Option<Entity<InputState>>,
        indicator_input: Entity<InputState>,
        timeframe_input: Entity<InputState>,
        time_zone_input: Entity<InputState>,
        chart_chrome: chart_chrome::ChartChromePreferences,
        restored_chart_state: Option<WorkspaceChartState>,
    ) -> Self {
        let theme = AerisTheme::dark();
        let restored_rithmic = restored_market_selection(&startup);
        let symbol_browser = initial_symbol_browser(&startup);
        let startup_state = terminal_startup_state(startup, cx);
        let interval = restored_rithmic.map_or(ChartInterval::Minute1, |restored| restored.0);
        let (studies, price_alerts, price_alert_message) =
            restored_workspace_records(restored_chart_state.as_ref(), &market_worker);
        initialize_restored_chart(startup_state.chart.as_ref(), chart_chrome, cx);
        replace_chart_price_alert_lines(
            startup_state.chart.as_ref(),
            &price_alerts,
            startup_state.product.as_ref(),
            cx,
        );
        let bridge_label = chart_bridge_label(startup_state.chart.as_ref(), cx);
        let order_book = cx.new(move |_| ReadOnlyOrderBookView::new(theme));
        let context_view = lifecycle.context_view();
        let context_snapshot = context_view.as_ref().map_or_else(
            || Arc::new(ContextSnapshot::empty(1)),
            ContextView::snapshot,
        );
        let (context_panel_visible, context_panel_tab, chart_link_group, chart_link_flags) =
            restored_context_and_link_state(restored_chart_state.as_ref());
        let context_panel_height = restored_context_panel_height(restored_chart_state.as_ref());
        let chart_time_zone = restored_chart_time_zone(restored_chart_state.as_ref());
        let chart_trading_visibility =
            restored_chart_trading_visibility(restored_chart_state.as_ref());
        if let Some(chart) = startup_state.chart.as_ref() {
            // A chart created at startup never takes the restored-state path.
            let time_zone = chart_time_zone.clone();
            chart.update(cx, |chart, _| {
                if let Err(error) = chart.set_time_zone(&time_zone) {
                    eprintln!("Aeris chart time zone could not be applied: {error}");
                }
            });
        }
        workspace_surface_from_initialization(WorkspaceSurfaceInitialization {
            startup: startup_state,
            market_worker,
            lifecycle,
            symbol_input,
            indicator_input,
            timeframe_input,
            time_zone_input,
            chart_chrome,
            restored_chart_state,
            restored_rithmic,
            interval,
            symbol_browser,
            studies,
            price_alerts,
            price_alert_message,
            bridge_label,
            order_book,
            context_view,
            context_snapshot,
            context_panel_visible,
            context_panel_tab,
            context_panel_height,
            chart_link_group,
            chart_link_flags,
            chart_time_zone,
            chart_trading_visibility,
            chrome_focus: cx.focus_handle().tab_stop(true),
            theme,
        })
    }

    pub(super) fn retire_market_worker(&mut self, cx: &App) {
        if let Some(retirement) = self.market_worker.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }

    pub(super) fn set_market_resource_class(&mut self, resource_class: ConsumerResourceClass) {
        self.resource_class = resource_class;
        let _ = self
            .market_worker
            .try_set_market_resource_class(resource_class);
    }

    pub(super) fn set_market_message_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.market_worker.set_message_wake(wake);
    }

    pub(super) fn should_poll_market(&self) -> bool {
        self.resource_class == ConsumerResourceClass::Foreground
    }

    pub(super) fn available_intervals(&self) -> &'static [ChartInterval] {
        super::provider_intervals(self.provider)
    }

    pub(super) fn quick_timeframe_matches(&self, cx: &App) -> Vec<ChartInterval> {
        let query = self.timeframe_input.read(cx).value();
        self.available_intervals()
            .iter()
            .copied()
            .filter(|interval| interval.matches_typeahead(query.as_ref()))
            .collect()
    }

    fn sync_timeframe_menu_selection(&mut self) {
        self.timeframe_menu_flyout = None;
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        let selected_group = timeframe_interval_group(self.selected_interval());
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|group| *group == selected_group)
            .unwrap_or(0);
    }

    pub(super) fn open_timeframe_group(
        &mut self,
        group: TimeframeMenuGroup,
        from_keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        self.retain_timeframe_flyout();
        let already_open = self.timeframe_menu_flyout == Some(group);
        if !already_open {
            self.timeframe_menu_flyout = Some(group);
            self.chrome_selection = timeframe_group_intervals(group, self.available_intervals())
                .iter()
                .position(|interval| *interval == self.selected_interval())
                .unwrap_or(0);
        }
        if self.menu_state.timeframe_flyout_keyboard != from_keyboard || !already_open {
            self.menu_state.timeframe_flyout_keyboard = from_keyboard;
            cx.notify();
        }
    }

    fn retain_timeframe_flyout(&mut self) {
        self.timeframe_flyout_close_token = self.timeframe_flyout_close_token.saturating_add(1);
    }

    fn arm_timeframe_flyout_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_timeframe_flyout();
        let token = self.timeframe_flyout_close_token;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if app.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && app.timeframe_flyout_close_token == token
                {
                    app.close_timeframe_flyout(app_cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn hover_timeframe_menu_region(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.timeframe_hover_regions = self.timeframe_hover_regions.saturating_add(1);
            self.retain_timeframe_flyout();
            return;
        }
        if self.timeframe_hover_regions == 0 {
            return;
        }
        self.timeframe_hover_regions -= 1;
        if self.timeframe_hover_regions == 0 {
            self.arm_timeframe_flyout_close(window, cx);
        }
    }

    fn close_timeframe_flyout(&mut self, cx: &mut Context<Self>) {
        let Some(group) = self.timeframe_menu_flyout.take() else {
            return;
        };
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|item| *item == group)
            .unwrap_or(0);
        cx.notify();
    }

    fn timeframe_menu_keyboard_count(&self) -> usize {
        if let Some(group) = self.timeframe_menu_flyout {
            timeframe_group_intervals(group, self.available_intervals()).len()
        } else {
            timeframe_menu_groups(self.available_intervals()).len()
        }
    }

    fn sync_chart_type_menu_selection(&mut self, cx: &App) {
        let selected = self.chart_type(cx);
        self.chrome_selection = ChartType::ALL
            .iter()
            .position(|chart_type| *chart_type == selected)
            .unwrap_or(0);
    }

    fn apply_highlighted_chart_type(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chart_type) = ChartType::ALL.get(self.chrome_selection).copied() {
            self.set_chart_type(chart_type, cx);
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn time_zone_matches(&self, cx: &App) -> Vec<&'static str> {
        let query = self
            .time_zone_input
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let utc_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);
        AerisChartView::supported_time_zones()
            .iter()
            .copied()
            .filter(|zone| {
                if query.is_empty() {
                    return true;
                }
                let normalized = zone.replace(['_', '/'], " ").to_ascii_lowercase();
                let badge = AerisChartView::time_zone_badge_label(zone, utc_seconds)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                zone.to_ascii_lowercase().contains(&query)
                    || normalized.contains(&query)
                    || badge.contains(&query)
            })
            .collect()
    }

    /// Restored and retained snapshots can predate the trader's latest choice, so the
    /// surface-owned zone is applied last to every chart this pane creates.
    fn apply_surface_time_zone(&self, chart: &Entity<AerisChartView>, cx: &mut Context<Self>) {
        let time_zone = self.chart_time_zone.clone();
        chart.update(cx, |chart, _| {
            if let Err(error) = chart.set_time_zone(&time_zone) {
                eprintln!("Aeris chart time zone could not be applied: {error}");
            }
        });
    }

    /// The selected chart time zone, owned by the surface so it reads correctly before the
    /// chart is created and across chart replacement.
    pub(super) fn chart_time_zone_id(&self) -> &str {
        &self.chart_time_zone
    }

    /// Header clock in the pane's selected zone. It needs no chart, so it shows at startup.
    pub(super) fn chart_time_zone_clock(&self) -> String {
        let utc_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|now| i64::try_from(now.as_secs()).ok())
            .unwrap_or(0);
        AerisChartView::time_zone_clock_label_for(&self.chart_time_zone, utc_seconds)
    }

    fn sync_time_zone_menu_selection(&mut self, cx: &App) {
        let selected = self.chart_time_zone_id();
        self.chrome_selection = self
            .time_zone_matches(cx)
            .iter()
            .position(|zone| *zone == selected)
            .unwrap_or(0);
    }

    pub(super) fn set_chart_time_zone(&mut self, time_zone: &str, cx: &mut Context<Self>) -> bool {
        if !aeris_chart_integration::TRADINGVIEW_TIME_ZONES.contains(&time_zone) {
            self.indicator_message = Some(format!("Unsupported chart time zone: {time_zone}"));
            return false;
        }
        if let Some(chart) = &self.chart
            && let Err(error) = chart.update(cx, |chart, chart_cx| {
                let changed = chart.set_time_zone(time_zone)?;
                if changed {
                    chart_cx.notify();
                }
                Ok::<_, String>(changed)
            })
        {
            self.indicator_message = Some(error);
            return false;
        }
        if self.chart_time_zone != time_zone {
            self.chart_time_zone = time_zone.to_string();
            self.chart_persistence_dirty = true;
            cx.notify();
        }
        true
    }

    pub(super) fn apply_highlighted_time_zone(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(time_zone) = self
            .time_zone_matches(cx)
            .get(self.chrome_selection)
            .copied()
            && self.set_chart_time_zone(time_zone, cx)
        {
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn sync_quick_timeframe_selection(&mut self, cx: &App) {
        let query = self.timeframe_input.read(cx).value();
        let intervals = self.quick_timeframe_matches(cx);
        self.chrome_selection = intervals
            .iter()
            .position(|interval| interval.label() == query.as_ref())
            .or_else(|| {
                intervals
                    .iter()
                    .position(|interval| *interval == self.selected_interval())
            })
            .unwrap_or(0);
    }

    /// Applies the typed quick timeframe. A query that matches no offered interval, or an
    /// interval this surface refuses (plan limit, no market), keeps the popup open and shows
    /// why instead of closing silently.
    pub(super) fn submit_quick_timeframe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.timeframe_input.read(cx).value().to_string();
        let intervals = self.quick_timeframe_matches(cx);
        if intervals.is_empty() {
            let message = format!(
                "\u{201c}{query}\u{201d} is not an available timeframe. Try 1m, 5m, 1H or 1D."
            );
            self.menu_state.quick_timeframe_error = Some(QuickTimeframeError { query, message });
            cx.notify();
            return;
        }
        self.apply_highlighted_interval(&intervals, window, cx);
        let still_open = self.chrome_overlay == Some(ChromeOverlay::QuickTimeframe)
            && self.chrome_overlay_phase != ChromeOverlayPhase::Closing;
        if still_open {
            let message = if self.series_message.is_empty() {
                "This timeframe could not be applied.".to_string()
            } else {
                self.series_message.clone()
            };
            self.menu_state.quick_timeframe_error = Some(QuickTimeframeError { query, message });
            cx.notify();
        }
    }

    pub(super) fn apply_highlighted_interval(
        &mut self,
        intervals: &[ChartInterval],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(interval) = intervals.get(self.chrome_selection).copied()
            && self.select_interval(interval, cx)
        {
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn selected_interval(&self) -> ChartInterval {
        self.interval
    }

    pub(super) fn select_interval(
        &mut self,
        interval: ChartInterval,
        cx: &mut Context<Self>,
    ) -> bool {
        let starter_interval = matches!(
            interval,
            ChartInterval::Minute1
                | ChartInterval::Minute3
                | ChartInterval::Minute5
                | ChartInterval::Minute15
                | ChartInterval::Minute30
                | ChartInterval::Hour1
                | ChartInterval::Hour2
                | ChartInterval::Hour4
                | ChartInterval::Hour8
                | ChartInterval::Hour12
                | ChartInterval::Day1
        );
        if !current_plan_limits().extended_timeframes && !starter_interval {
            self.series_message = "This timeframe requires a paid plan".to_string();
            cx.notify();
            return false;
        }
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| {
            if self.interval == interval && self.rithmic_pending_interval.is_none() {
                return true;
            }
            if self.rithmic_pending_interval == Some(interval) {
                return true;
            }
            let Some(product) = self.product.clone() else {
                let display = terminal_provider_display(self.provider);
                self.series_message = format!("{display} market selection is unavailable");
                cx.notify();
                return false;
            };
            let Ok(sequence) = self.market_worker.try_select_engine(product, interval) else {
                self.series_message = format!("{} history could not start", interval.label());
                cx.notify();
                return false;
            };
            self.rithmic_pending_interval = Some(interval);
            self.rithmic_pending_sequence = Some(sequence);
            self.rithmic_switch = RithmicSwitchState::Pending;
            self.chart_state = ChartState::Loading;
            self.chart_state_message = format!("Loading {} history", interval.label());
            self.series_message = format!("Switching to {}", interval.label());
            cx.notify();
            true
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_interval_selection(elapsed_nanos(started));
        selected
    }

    pub(super) fn select_installed_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.product.as_ref().is_some_and(|current| {
            current.provider == instrument.provider
                && current.instrument_id == instrument.instrument_id
        }) && self.rithmic_pending_product.is_none()
        {
            return true;
        }
        if self
            .rithmic_pending_product
            .as_ref()
            .is_some_and(|pending| {
                pending.provider == instrument.provider
                    && pending.instrument_id == instrument.instrument_id
            })
        {
            return true;
        }
        let interval = self.interval;
        let Ok(sequence) = self
            .market_worker
            .try_select_engine(instrument.clone(), interval)
        else {
            self.series_message = format!(
                "{} market history could not start",
                instrument.display_symbol
            );
            cx.notify();
            return false;
        };
        self.rithmic_pending_product = Some(instrument.clone());
        self.pending_linked_instrument = Some(instrument.clone());
        self.rithmic_pending_interval = Some(interval);
        self.rithmic_pending_sequence = Some(sequence);
        self.rithmic_switch = RithmicSwitchState::Pending;
        self.chart_state = ChartState::Loading;
        self.chart_state_message = format!("Loading {} market history", instrument.display_symbol);
        self.series_message = format!("Switching to {}", instrument.display_symbol);
        cx.notify();
        true
    }
    pub(super) fn choose_symbol_provider(
        &mut self,
        provider: TerminalProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.market_state.symbol_selection_pending {
            return;
        }
        self.menu_state.symbol_provider_menu = SymbolProviderMenu::Closed;
        if self.symbol_provider != provider {
            self.symbol_provider = provider;
            self.symbol_browser.change_catalog();
            self.pending_mnemonic_symbol = None;
            self.chrome_selection = 0;
        }
        let query = default_listing_query(provider);
        if let Some(input) = &self.symbol_input {
            input.update(cx, |input, input_cx| {
                input.set_value(query, window, input_cx);
            });
        }
        self.search_symbol_query(query, cx);
        cx.notify();
    }

    pub(super) fn toggle_symbol_provider_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_state.symbol_provider_menu = self.menu_state.symbol_provider_menu.toggled();
        cx.notify();
    }

    /// Shows or hides the Markets flyout beside the open provider menu, the way timeframe
    /// groups open their flyouts: the Markets row opens it, other rows close it.
    pub(super) fn set_symbol_markets_flyout(&mut self, open: bool, cx: &mut Context<Self>) {
        let next = match (self.menu_state.symbol_provider_menu, open) {
            (SymbolProviderMenu::Closed, _) => return,
            (_, true) => SymbolProviderMenu::MarketsFlyout,
            (_, false) => SymbolProviderMenu::Open,
        };
        if self.menu_state.symbol_provider_menu != next {
            self.menu_state.symbol_provider_menu = next;
            cx.notify();
        }
    }

    /// Includes or excludes one instrument category from symbol search and re-runs the current
    /// query. The last included category cannot be excluded, so search always returns something.
    pub(super) fn toggle_symbol_search_category(
        &mut self,
        category: SymbolSearchCategory,
        cx: &mut Context<Self>,
    ) {
        let mut categories = self.chart_chrome.symbol_search_categories;
        match category {
            SymbolSearchCategory::Futures => categories.futures = !categories.futures,
            SymbolSearchCategory::Equities => categories.equities = !categories.equities,
        }
        if !categories.futures && !categories.equities {
            return;
        }
        self.chart_chrome.symbol_search_categories = categories;
        self.save_chart_chrome_preferences(cx);
        self.search_symbol_input(cx);
        cx.notify();
    }

    pub(super) fn instrument_entries(&self, _cx: &App) -> Vec<InstrumentMenuEntry> {
        self.symbol_browser
            .results()
            .iter()
            .enumerate()
            .map(|(index, instrument)| InstrumentMenuEntry {
                label: instrument.display_symbol.clone(),
                checked: self.symbol_browser.selected().is_some_and(|selected| {
                    selected.instrument.symbol == instrument.symbol
                        && selected.instrument.exchange == instrument.exchange
                }),
                selection: InstrumentMenuSelection(index),
            })
            .collect()
    }

    fn activate_chrome_list_keyboard(&mut self) {
        if matches!(
            self.chrome_overlay,
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator | ChromeOverlay::TimeZone)
        ) {
            self.menu_state.chrome_list_keyboard = true;
        }
    }

    fn scroll_chrome_selection_into_view(&self, cx: &App) {
        match self.chrome_overlay {
            Some(ChromeOverlay::Instrument) if !self.symbol_browser.results().is_empty() => {
                self.scrolls
                    .instrument
                    .scroll_to_item(self.chrome_selection);
            }
            Some(ChromeOverlay::Indicator)
                if !chart_chrome::filter_indicator_specs(
                    self.indicator_input.read(cx).value().as_ref(),
                )
                .is_empty() =>
            {
                // The group heading is the scroll body's first child.
                self.scrolls
                    .indicator
                    .scroll_to_item(self.chrome_selection.saturating_add(1));
            }
            Some(ChromeOverlay::TimeZone) if !self.time_zone_matches(cx).is_empty() => {
                self.scrolls.time_zone.scroll_to_item(self.chrome_selection);
            }
            _ => {}
        }
    }

    fn sync_instrument_menu_keyboard(&mut self, cx: &App) {
        if self.menu_state.chrome_list_keyboard {
            return;
        }
        self.chrome_selection =
            current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0);
    }

    pub(super) fn select_instrument(
        &mut self,
        selection: InstrumentMenuSelection,
        target: SymbolSelectionTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        if target == SymbolSelectionTarget::Watchlist && self.market_state.symbol_selection_pending
        {
            return false;
        }
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| {
            let index = selection.0;
            let provider = terminal_provider_id(self.symbol_provider);
            let Some(descriptor) = super::provider_presentation(self.symbol_provider) else {
                return false;
            };
            let display = terminal_provider_display(self.symbol_provider);
            let Some(selection) = self.symbol_browser.select(index) else {
                return false;
            };
            let request = SelectProviderInstrument {
                consumer_id: 0,
                selection_generation: selection.generation.get() as u64,
                search_generation: selection.search_generation.get() as u64,
                provider: provider.to_string(),
                symbol: selection.instrument.symbol.clone(),
                exchange: selection.instrument.exchange.clone(),
                entitlement_id: descriptor.selection_entitlement_id.to_string(),
            };
            if self.market_worker.try_select_provider(request).is_err() {
                self.symbol_browser.reject_selection(selection.generation);
                self.symbol_message = format!("{display} market selection is busy; try again");
                cx.notify();
                return false;
            }
            self.pending_symbol_selection_target = Some(target);
            self.market_state.symbol_selection_pending = true;
            self.symbol_message = format!("Selecting {}", selection.instrument.display_symbol);
            cx.notify();
            true
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_instrument_selection(elapsed_nanos(started));
        selected
    }

    pub(super) fn open_chrome_overlay(
        &mut self,
        overlay: ChromeOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chrome_overlay_with_trigger(overlay, None, window, cx);
    }

    pub(super) fn open_chrome_overlay_at(
        &mut self,
        overlay: ChromeOverlay,
        trigger_position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chrome_overlay_with_trigger(overlay, Some(trigger_position), window, cx);
    }

    fn open_chrome_overlay_with_trigger(
        &mut self,
        overlay: ChromeOverlay,
        trigger_position: Option<gpui::Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if overlay == ChromeOverlay::Instrument {
            self.symbol_selection_target = SymbolSelectionTarget::Chart;
        }
        if matches!(overlay, ChromeOverlay::Instrument | ChromeOverlay::Accounts) {
            // Both surfaces present the broker connection; the runtime check reads the
            // local credential vault in the background and never blocks this frame.
            window.dispatch_action(Box::new(RefreshTastytradeConnection), cx);
        }
        self.chrome_overlay_trigger_position = trigger_position;
        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Opening;
        self.chrome_overlay = Some(overlay);
        if overlay != ChromeOverlay::QuickTimeframe {
            self.timeframe_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        if overlay == ChromeOverlay::TimeZone {
            self.time_zone_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        if overlay != ChromeOverlay::Timeframe {
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.timeframe_hover_regions = 0;
        }
        self.menu_state.chrome_list_keyboard = false;
        self.chrome_selection = match overlay {
            ChromeOverlay::Timeframe => {
                self.sync_timeframe_menu_selection();
                self.chrome_selection
            }
            ChromeOverlay::ChartType => {
                self.sync_chart_type_menu_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::QuickTimeframe => {
                self.sync_quick_timeframe_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::Instrument => {
                current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0)
            }
            ChromeOverlay::Indicator | ChromeOverlay::Accounts => 0,
            ChromeOverlay::TimeZone => {
                self.sync_time_zone_menu_selection(cx);
                self.chrome_selection
            }
        };
        match overlay {
            ChromeOverlay::Instrument => {
                self.scrolls.instrument.set_offset(point(px(0.0), px(0.0)));
                if let Some(input) = &self.symbol_input {
                    input.update(cx, |input, input_cx| input.focus(window, input_cx));
                }
                self.refresh_default_instrument_listing(cx);
                self.sync_instrument_menu_keyboard(cx);
            }
            ChromeOverlay::Indicator => {
                self.scrolls.indicator.set_offset(point(px(0.0), px(0.0)));
                self.indicator_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::QuickTimeframe => {
                self.timeframe_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::TimeZone => {
                self.scrolls.time_zone.set_offset(point(px(0.0), px(0.0)));
                self.time_zone_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::Timeframe | ChromeOverlay::ChartType | ChromeOverlay::Accounts => {
                self.chrome_focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn close_chrome_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return;
        }
        if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
            self.menu_state.symbol_provider_menu = SymbolProviderMenu::Closed;
            self.symbol_selection_target = instrument_target_after_close(
                self.symbol_selection_target,
                self.market_state.symbol_selection_pending,
            );
        }
        match self.chrome_overlay {
            Some(ChromeOverlay::Indicator) => {
                self.indicator_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(ChromeOverlay::QuickTimeframe) => {
                self.menu_state.quick_timeframe_error = None;
                self.timeframe_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(ChromeOverlay::TimeZone) => {
                self.time_zone_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(ChromeOverlay::Accounts) => {
                // An unfinished form or confirmation never outlives the panel that owns it.
                self.trading_pnl.account_creator = None;
                self.trading_pnl.account_delete_confirmation = None;
            }
            Some(
                ChromeOverlay::Instrument | ChromeOverlay::Timeframe | ChromeOverlay::ChartType,
            )
            | None => {}
        }
        self.chrome_focus.focus(window, cx);
        if cx.reduce_motion() {
            self.chrome_overlay = None;
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.menu_state.chrome_list_keyboard = false;
            self.timeframe_hover_regions = 0;
            cx.notify();
            return;
        }

        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Closing;
        let generation = self.chrome_overlay_generation;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(CHROME_OVERLAY_EXIT_DURATION)
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if should_finish_chrome_overlay_close(
                    app.chrome_overlay_phase,
                    app.chrome_overlay_generation,
                    generation,
                ) {
                    app.chrome_overlay = None;
                    app.timeframe_menu_flyout = None;
                    app.menu_state.timeframe_flyout_keyboard = false;
                    app.menu_state.chrome_list_keyboard = false;
                    app.timeframe_hover_regions = 0;
                    app.chrome_overlay_phase = ChromeOverlayPhase::Opening;
                    app_cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.trading_pnl.account_creator.is_some() {
            if event.keystroke.key.eq_ignore_ascii_case("escape") {
                self.trading_pnl.account_creator = None;
                cx.notify();
                return true;
            }
            // Text editing inside the modal owns ordinary key input. In particular, do not let
            // the workspace-wide symbol/timeframe typeahead treat account-name characters as
            // chart shortcuts after the Input has received them.
            return false;
        }
        if self.trading_pnl.account_delete_confirmation.is_some() {
            if event.keystroke.key.eq_ignore_ascii_case("escape") {
                self.trading_pnl.account_delete_confirmation = None;
                cx.notify();
                return true;
            }
            return false;
        }
        if let Some(command) =
            fullscreen_escape_command(event.keystroke.key.as_str(), window.is_fullscreen())
        {
            command.execute(window);
            return true;
        }
        if self.consume_chrome_typeahead(event, window, cx) {
            return true;
        }
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                {
                    self.close_timeframe_flyout(cx);
                } else {
                    self.close_chrome_overlay(window, cx);
                }
            }
            "up" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                self.activate_chrome_list_keyboard();
                self.chrome_selection = self.chrome_selection.saturating_sub(1);
                self.scroll_chrome_selection_into_view(cx);
                cx.notify();
            }
            "down" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                let count = match self.chrome_overlay {
                    Some(ChromeOverlay::Instrument) => self.instrument_entries(cx).len(),
                    Some(ChromeOverlay::Indicator) => chart_chrome::filter_indicator_specs(
                        self.indicator_input.read(cx).value().as_ref(),
                    )
                    .len(),
                    Some(ChromeOverlay::Timeframe) => self.timeframe_menu_keyboard_count(),
                    Some(ChromeOverlay::ChartType) => ChartType::ALL.len(),
                    Some(ChromeOverlay::QuickTimeframe) => self.quick_timeframe_matches(cx).len(),
                    Some(ChromeOverlay::TimeZone) => self.time_zone_matches(cx).len(),
                    // The Accounts panel is a form, not a keyboard-navigated list.
                    Some(ChromeOverlay::Accounts) | None => 0,
                };
                self.activate_chrome_list_keyboard();
                self.chrome_selection = (self.chrome_selection + 1).min(count.saturating_sub(1));
                self.scroll_chrome_selection_into_view(cx);
                cx.notify();
            }
            "left" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                self.close_timeframe_flyout(cx);
            }
            "right" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                if self.timeframe_menu_flyout.is_none()
                    && let Some(group) = timeframe_menu_groups(self.available_intervals())
                        .get(self.chrome_selection)
                        .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            "enter" => return self.handle_chrome_enter(window, cx),
            _ => return false,
        }
        true
    }

    fn handle_chrome_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.chrome_overlay {
            Some(ChromeOverlay::Timeframe) => {
                if let Some(group) = self.timeframe_menu_flyout {
                    if self.menu_state.timeframe_flyout_keyboard {
                        let intervals =
                            timeframe_group_intervals(group, self.available_intervals());
                        self.apply_highlighted_interval(&intervals, window, cx);
                    } else {
                        self.menu_state.timeframe_flyout_keyboard = true;
                        cx.notify();
                    }
                } else if let Some(group) = timeframe_menu_groups(self.available_intervals())
                    .get(self.chrome_selection)
                    .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            Some(ChromeOverlay::ChartType) => self.apply_highlighted_chart_type(window, cx),
            Some(ChromeOverlay::TimeZone | ChromeOverlay::QuickTimeframe) => {
                // InputState emits PressEnter before propagating the raw key.
                // Its subscription owns submission: handling both would submit
                // again after closing clears the query, selecting another interval.
                return false;
            }
            // Accounts form inputs own their own Enter handling.
            Some(
                ChromeOverlay::Instrument | ChromeOverlay::Indicator | ChromeOverlay::Accounts,
            )
            | None => return false,
        }
        true
    }

    fn consume_chrome_typeahead(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if chrome_typeahead_blocked(event) {
            return false;
        }
        if self.chrome_overlay_phase == ChromeOverlayPhase::Closing {
            return false;
        }
        if self.drawing_toolbar_state(cx).active_tool == Some(ChartDrawingKind::Text) {
            return false;
        }
        if self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).is_editing_text())
        {
            return false;
        }
        let Some(typed) = chrome_typeahead_char(event) else {
            return false;
        };
        if event.is_held && self.chrome_overlay.is_none() {
            return false;
        }
        match self.chrome_overlay {
            None if typed.is_ascii_digit() => {
                self.begin_quick_timeframe(typed, window, cx);
                true
            }
            None if typed.is_ascii_alphabetic() => self.begin_symbol_typeahead(typed, window, cx),
            Some(
                ChromeOverlay::Instrument
                | ChromeOverlay::Indicator
                | ChromeOverlay::Timeframe
                | ChromeOverlay::QuickTimeframe
                | ChromeOverlay::ChartType
                | ChromeOverlay::TimeZone
                | ChromeOverlay::Accounts,
            )
            | None => false,
        }
    }

    fn begin_quick_timeframe(&mut self, typed: char, window: &mut Window, cx: &mut Context<Self>) {
        self.timeframe_input.update(cx, |input, input_cx| {
            input.set_value(typed.to_string(), window, input_cx);
            input.focus(window, input_cx);
        });
        self.open_chrome_overlay(ChromeOverlay::QuickTimeframe, window, cx);
    }

    fn begin_symbol_typeahead(
        &mut self,
        typed: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(input) = self.symbol_input.clone() else {
            return false;
        };
        let query = typed.to_string();
        input.update(cx, |input, input_cx| {
            input.set_value(&query, window, input_cx);
            input.focus(window, input_cx);
        });
        self.search_symbol_query(&query, cx);
        self.open_chrome_overlay(ChromeOverlay::Instrument, window, cx);
        true
    }

    #[cfg(feature = "diagnostics")]
    fn record_live_evidence_publication(&mut self, update: &ReplayStreamUpdate) {
        if !self.live_evidence_enabled || self.live_evidence_publications >= 256 {
            return;
        }
        self.live_evidence_publications = self.live_evidence_publications.saturating_add(1);
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                let interval_nanos = (snapshot.bar_definition().interval_seconds > 0).then(|| {
                    i64::from(snapshot.bar_definition().interval_seconds)
                        .saturating_mul(1_000_000_000)
                });
                let interior_gaps = interval_nanos.map(|interval_nanos| {
                    snapshot
                        .bars()
                        .windows(2)
                        .filter(|pair| {
                            pair[1]
                                .provenance()
                                .exchange_timestamp_unix_nanos
                                .saturating_sub(pair[0].provenance().exchange_timestamp_unix_nanos)
                                != interval_nanos
                        })
                        .count()
                });
                let first_timestamp = snapshot
                    .bars()
                    .first()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let last_timestamp = snapshot
                    .bars()
                    .last()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let interior_gaps =
                    interior_gaps.map_or_else(|| "null".to_string(), |value| value.to_string());
                let interval_nanos =
                    interval_nanos.map_or_else(|| "null".to_string(), |value| value.to_string());
                eprintln!(
                    "AERIS_LIVE_SNAPSHOT {{\"bar_count\":{},\"first_timestamp\":{first_timestamp},\"last_timestamp\":{last_timestamp},\"interior_gaps\":{interior_gaps},\"interval_nanos\":{interval_nanos}}}",
                    snapshot.bars().len()
                );
            }
            ReplayStreamUpdate::Delta(delta) => eprintln!(
                "AERIS_LIVE_UPDATE {{\"kind\":\"delta\",\"timestamp\":{}}}",
                delta.item().provenance().exchange_timestamp_unix_nanos
            ),
            ReplayStreamUpdate::Tail(tail) => eprintln!(
                "AERIS_LIVE_UPDATE {{\"kind\":\"tail\",\"timestamp\":{},\"forming\":{}}}",
                tail.item().provenance().exchange_timestamp_unix_nanos,
                tail.forming()
            ),
        }
    }

    pub(super) fn apply_publication(
        &mut self,
        publication: MarketWorkerPublication,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "diagnostics")]
        self.record_live_evidence_publication(&publication.update);
        let MarketWorkerPublication {
            update,
            generation,
            subscription_id,
            worker_label,
            ui_diagnostics,
        } = publication;
        self.worker_label = worker_label;
        self.subscription_id = subscription_id;
        self.replay_label =
            generation_status(&self.worker_label, &self.subscription_id, generation);
        // A switch that has been committed but not yet drawn keeps the previous
        // chart on screen. That chart belongs to the previous series, so the
        // replacement's incremental updates must not reach it; only its covering
        // snapshot may, and that snapshot is what swaps the chart.
        let swapping = self.rithmic_switch.is_swapping();
        let next_state = match (&self.chart, update) {
            (existing, aeris_application::ReplayStreamUpdate::Snapshot(snapshot))
                if existing.is_none() || swapping =>
            {
                let chart_theme = aeris_chart_theme(self.theme.mode);
                let chart =
                    cx.new(move |_| AerisChartView::with_replay_and_theme(&snapshot, chart_theme));
                self.apply_chart_chrome_to_chart(&chart, cx);
                if let Some(restored) = self.restored_chart_state.take() {
                    Self::apply_restored_chart_state(&chart, &restored, true, cx);
                } else {
                    self.apply_retained_chart_state_to_chart(&chart, cx);
                }
                self.apply_surface_time_zone(&chart, cx);
                self.apply_market_session_to_chart(&chart, cx);
                replace_chart_price_alert_lines(
                    Some(&chart),
                    &self.price_alerts,
                    self.product.as_ref(),
                    cx,
                );
                if let Some((start, end)) = self.restored_viewport {
                    chart.update(cx, |chart, _| {
                        chart.set_visible_time_range_unix_nanos(start, end);
                    });
                }
                self.last_chart_user_state_revision = chart.read(cx).user_state_revision();
                self.last_chart_clock_revision = chart.read(cx).clock_revision();
                observe_chart(Some(&chart), cx);
                self.chart = Some(chart);
                self.synchronize_runtime_studies(cx);
                self.rithmic_switch = RithmicSwitchState::Initializing;
                ChartState::Ready
            }
            (Some(_), _) if swapping => {
                // The replacement has not arrived yet; the previous chart stays
                // as it is rather than being fed another series' bars.
                return;
            }
            (Some(chart), update) => {
                let (accepted, recovery_pending) = chart.update(cx, |chart, _| {
                    let accepted = chart.try_queue_replay_update(update).is_ok();
                    if !accepted {
                        eprintln!("bounded chart queue overflowed; canonical resnapshot required");
                    }
                    (accepted, chart.replay_bridge_metrics().recovery_pending)
                });
                publication_chart_state(accepted, recovery_pending)
            }
            (None, _) => {
                self.reject_incremental_publication(ui_diagnostics, cx);
                return;
            }
        };
        if let Some(diagnostics) = ui_diagnostics
            && let Some(replaced) = self.pending_ui_diagnostics.replace(diagnostics)
        {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: replaced.generation(),
                });
        }
        // A publication says bars arrived, not that they are current. The engine
        // reports readiness separately, and while it is still loading current
        // coverage the chart is showing retained history — promoting it here is
        // what presented a stale chart as ready for the seconds before the
        // provider page and the live handoff landed.
        if next_state == ChartState::Ready && self.chart_state.awaits_engine_readiness() {
            cx.notify();
        } else if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
            if super::provider_presentation(self.provider).is_some() {
                self.market_state.symbol_selection_pending = false;
                self.symbol_message = self.product.as_ref().map_or_else(
                    || {
                        format!(
                            "{} market ready",
                            super::terminal_provider_display(self.provider)
                        )
                    },
                    |product| super::provider_ready_message(self.provider, product),
                );
            }
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
    }

    fn reject_incremental_publication(
        &mut self,
        diagnostics: Option<PendingUiDiagnostics>,
        cx: &mut Context<Self>,
    ) {
        if let Some(diagnostics) = diagnostics {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: diagnostics.generation(),
                });
        }
        self.set_chart_state(
            ChartState::Error,
            "market update arrived before the initial covering snapshot".to_string(),
            cx,
        );
    }

    fn apply_recovery(
        &mut self,
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                let current = self.chart.as_ref().is_some_and(|chart| {
                    chart.update(cx, |chart, chart_cx| {
                        let current = chart.mark_replay_recovery_failed(request_id);
                        if current {
                            chart_cx.notify();
                        }
                        current
                    })
                });
                if !current {
                    return;
                }
                self.apply_market_state_message(ChartState::Error, error.clone(), cx);
                eprintln!("fixture recovery {request_id} failed: {error}");
                return;
            }
        };
        let Some(chart) = &self.chart else {
            self.set_chart_state(
                ChartState::Error,
                "recovery response arrived before the initial snapshot".to_string(),
                cx,
            );
            return;
        };
        let install = chart.update(cx, |chart, chart_cx| {
            let installed = chart.install_replay_recovery(request_id, &bootstrap.snapshot);
            chart_cx.notify();
            installed
        });
        match install {
            Ok(true) => {
                self.replay_label = generation_status(
                    &self.worker_label,
                    &bootstrap.subscription_id,
                    MarketPublicationGeneration::from_generation(&bootstrap.generation),
                );
                self.apply_market_state_message(
                    ChartState::Ready,
                    "market snapshot is current".to_string(),
                    cx,
                );
            }
            Ok(false) => {
                let metrics = chart.read(cx).replay_bridge_metrics();
                if metrics.snapshot_required && !metrics.recovery_pending {
                    self.apply_market_state_message(
                        ChartState::Error,
                        "chart recovery exhausted its snapshot retry budget".to_string(),
                        cx,
                    );
                }
            }
            Err(error) => {
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                self.apply_market_state_message(ChartState::Error, error.to_string(), cx);
                eprintln!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.mark_replay_stream_invalid();
                chart_cx.notify();
            });
        }
        eprintln!("market worker invalidated the stream: {message}");
    }

    /// Reports whether an engine selection handoff must keep the chart covered.
    ///
    /// Before the replacement snapshot arrives, the pixels belong to the series
    /// the trader just left. After that first snapshot arrives, the replacement
    /// can still be retained/partial history until the provider-history/live
    /// handoff declares it current. Both phases are deliberately hidden behind
    /// the neutral loading surface so a stale range cannot appear to "wake up"
    /// and jump when current coverage lands.
    pub(super) fn showing_superseded_series(&self) -> bool {
        super::provider_presentation(self.provider).is_some()
            && switch_requires_chart_cover(self.chart.is_some(), self.rithmic_switch)
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
    }

    fn dispatch_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.market_worker.is_connected() {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let worker = &self.market_worker;
        let dispatch = chart.update(cx, |chart, chart_cx| {
            let result =
                chart.try_dispatch_replay_recovery(|command| worker.try_send_recovery(command));
            if result.as_ref().is_ok_and(|dispatched| *dispatched) {
                chart_cx.notify();
            }
            result
        });
        match dispatch {
            Ok(_) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(command)) => {
                self.market_worker.mark_disconnected();
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(command.request_id);
                    chart_cx.notify();
                });
            }
        }
    }

    fn apply_market_worker_message(
        &mut self,
        message: MarketWorkerMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            MarketWorkerMessage::Update(publication) => self.apply_publication(publication, cx),
            MarketWorkerMessage::Diagnostics(snapshot) => {
                #[cfg(feature = "diagnostics")]
                eprintln!("desktop market diagnostics: {snapshot:?}");
                #[cfg(not(feature = "diagnostics"))]
                drop(snapshot);
            }
            MarketWorkerMessage::Recovery { request_id, result } => {
                self.apply_recovery(request_id, result, cx);
            }
            MarketWorkerMessage::State { state, message } => {
                self.apply_market_state_message(state, message, cx);
            }
            MarketWorkerMessage::EngineSwitchMarker { sequence } => {
                self.apply_rithmic_switch_marker(sequence, cx);
            }
            MarketWorkerMessage::Connection {
                state,
                message,
                transport_rtt_nanos,
            } => {
                self.apply_market_connection_message(state, message, transport_rtt_nanos, cx);
            }
            MarketWorkerMessage::ProviderCatalog(event) => {
                self.apply_provider_catalog_event(event, cx);
            }
            MarketWorkerMessage::MarketSessionStatus(status) => {
                self.apply_market_session_status(status, cx);
            }
            MarketWorkerMessage::OrderBook(frame) => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.replace_frame(frame, order_book_cx)
                });
            }
            MarketWorkerMessage::TradeTape(snapshot) => self.apply_trade_tape(snapshot, cx),
            MarketWorkerMessage::DeltaDivergenceTriggered(trigger) => {
                self.apply_delta_divergence_trigger(&trigger, cx);
            }
            MarketWorkerMessage::StudyOutput(snapshot) => self.apply_study_output(&snapshot, cx),
            MarketWorkerMessage::StudyOutputsInvalidated(invalidated) => {
                self.apply_study_invalidated(&invalidated, cx);
            }
            MarketWorkerMessage::StudyRemoved(removed) => self.apply_study_removed(&removed, cx),
            MarketWorkerMessage::StudyRegistered {
                request_sequence,
                study_id,
            } => self.apply_study_registered(request_sequence, study_id, cx),
            MarketWorkerMessage::StudyReinitialized { study_id } => {
                self.apply_study_reinitialized(study_id, cx);
            }
            MarketWorkerMessage::StudyRegistrationFailed {
                request_sequence,
                message,
            } => self.apply_study_registration_failed(request_sequence, message, cx),
            MarketWorkerMessage::StudyReinitializationFailed { study_id, message } => {
                self.studies.cancel_reinitialization(study_id);
                if let Some(dialog) = &mut self.study_settings_dialog
                    && dialog.study_id == study_id
                {
                    dialog.message = Some(message.clone());
                }
                self.indicator_message = Some(message);
                cx.notify();
            }
            MarketWorkerMessage::StudyRemovalFailed { study_id, message } => {
                if study_removal_failed(&mut self.studies, study_id) {
                    self.indicator_message = None;
                } else {
                    self.indicator_message = Some(message);
                }
                cx.notify();
            }
            MarketWorkerMessage::PriceAlertTriggered(trigger) => {
                self.apply_price_alert_trigger(&trigger, cx);
            }
            MarketWorkerMessage::PriceAlertSyncFailed(error) => {
                self.price_alert_message = Some(error);
                cx.notify();
            }
            MarketWorkerMessage::ChartViewport {
                start_unix_nanos,
                end_unix_nanos,
            } => {
                let viewport = (start_unix_nanos, end_unix_nanos);
                self.restored_viewport = Some(viewport);
                self.last_persisted_viewport = Some(viewport);
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        if chart.set_visible_time_range_unix_nanos(start_unix_nanos, end_unix_nanos)
                        {
                            chart_cx.notify();
                        }
                    });
                }
            }
        }
    }

    fn apply_market_session_status(
        &mut self,
        status: aeris_contracts::MarketSessionStatus,
        cx: &mut Context<Self>,
    ) {
        // The header market indicator reads this for the selected product only.
        self.market_session_status = Some(status);
        if let Some(chart) = self.chart.clone() {
            self.apply_market_session_to_chart(&chart, cx);
        }
        cx.notify();
    }

    fn apply_market_session_to_chart(
        &self,
        chart: &Entity<AerisChartView>,
        cx: &mut Context<Self>,
    ) {
        let trading =
            chart_market_trading(self.market_session_status.as_ref(), self.product.as_ref());
        chart.update(cx, |chart, chart_cx| {
            chart.set_market_trading(trading);
            chart_cx.notify();
        });
    }

    fn apply_trade_tape(
        &mut self,
        snapshot: aeris_market_runtime::MarketTradeTapeSnapshot,
        cx: &mut Context<Self>,
    ) {
        let is_current = self.product.as_ref().is_none_or(|product| {
            snapshot.provider_id == product.provider
                && snapshot.instrument_id == product.instrument_id
                && snapshot.entitlement_id == product.entitlement_id
        });
        if is_current
            && self.trade_tape.as_ref().is_none_or(|current| {
                snapshot.generation.0.get() >= current.generation.0.get()
                    && snapshot.provider_generation >= current.provider_generation
                    && (current.generation != snapshot.generation
                        || current.provider_generation != snapshot.provider_generation
                        || snapshot.revision >= current.revision)
            })
        {
            self.apply_trade_tape_to_chart(&snapshot, cx);
            self.trade_tape = Some(snapshot);
            cx.notify();
        }
    }

    fn apply_delta_divergence_trigger(
        &mut self,
        trigger: &aeris_market_runtime::MarketDeltaDivergenceTrigger,
        cx: &mut Context<Self>,
    ) {
        let direction = match trigger.evidence.direction {
            aeris_market_runtime::DeltaDivergenceDirection::Bullish => "bullish",
            aeris_market_runtime::DeltaDivergenceDirection::Bearish => "bearish",
        };
        let symbol = self
            .product
            .as_ref()
            .map_or("contract", |product| product.display_symbol.as_str());
        let message = format!("{symbol} formed a {direction} price / cumulative-delta divergence");
        self.indicator_message = Some(message.clone());
        let notification = aeris_platform_runtime::NativeUserNotification::try_new(
            "Aeris order-flow alert",
            message,
        );
        if let Err(error) =
            notification.and_then(aeris_platform_runtime::try_send_user_notification)
        {
            eprintln!("Aeris order-flow notification was not delivered: {error}");
        }
        cx.notify();
    }

    fn current_runtime_series(&self) -> Result<BarSeriesKey, String> {
        let product = self
            .product
            .as_ref()
            .ok_or_else(|| "market selection is unavailable for this study".to_string())?;
        engine_market_worker::series_key(product, self.interval)
    }

    fn retry_automatic_study_removals(&mut self) -> bool {
        let market_worker = &self.market_worker;
        dispatch_automatic_study_removals(&mut self.studies, |study_id| {
            market_worker.try_remove_study(study_id)
        })
    }

    fn enqueue_runtime_study(
        &mut self,
        mut state: PendingRuntimeStudyState,
    ) -> Result<u64, Box<(PendingRuntimeStudyState, String)>> {
        if !self.retry_automatic_study_removals() {
            return Err(Box::new((
                state,
                "Study cancellation is waiting for runtime capacity".to_string(),
            )));
        }
        let current_series = match self.current_runtime_series() {
            Ok(series) => series,
            Err(error) => return Err(Box::new((state, error))),
        };
        let registration = match runtime_study_registration(
            &state.persisted,
            &current_series,
            &self.studies.active,
        ) {
            Ok(registration) => registration,
            Err(error) => {
                state.blocked = true;
                return Err(Box::new((state, error)));
            }
        };
        state.resolved_chart_series =
            runtime_study_uses_current_chart(&state.persisted).then_some(current_series);
        match self.market_worker.try_register_study(registration) {
            Ok(sequence) => {
                self.studies.pending.insert(sequence, state);
                Ok(sequence)
            }
            Err(TrySendError::Full(_)) => Err(Box::new((
                state,
                "Study request queue is busy; try again".to_string(),
            ))),
            Err(TrySendError::Disconnected(_)) => Err(Box::new((
                state,
                "Study runtime is unavailable".to_string(),
            ))),
        }
    }

    fn dispatch_deferred_runtime_studies(&mut self, cx: &mut Context<Self>) {
        if self.chart.is_none() || !self.studies.pending.is_empty() {
            return;
        }
        let Some(index) = next_deferred_runtime_study_index(&self.studies) else {
            return;
        };
        let state = self.studies.deferred.remove(index);
        if let Err(error) = self.enqueue_runtime_study(state) {
            let (state, message) = *error;
            self.studies.deferred.insert(index, state);
            self.indicator_message = Some(message);
        }
        if !self.studies.deferred.is_empty() {
            cx.notify();
        }
    }

    fn queue_runtime_study_reinitialization(
        &mut self,
        study_id: StudyInstanceId,
        series: BarSeriesKey,
    ) -> Result<(), String> {
        if !self.retry_automatic_study_removals() {
            return Err("Study cancellation is waiting for runtime capacity".to_string());
        }
        let persisted = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .map(|state| state.persisted.clone())
            .ok_or_else(|| "study is no longer active".to_string())?;
        let registration = runtime_study_registration(&persisted, &series, &self.studies.active)?;
        match self
            .market_worker
            .try_reinitialize_study(study_id, registration)
        {
            Ok(()) => {
                self.studies.begin_reinitialization(
                    study_id,
                    PendingStudyReinitialization {
                        series,
                        replacement_persisted: None,
                    },
                );
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                Err("Study reinitialization queue is busy; retrying".to_string())
            }
            Err(TrySendError::Disconnected(_)) => Err("Study runtime is unavailable".to_string()),
        }
    }

    fn synchronize_runtime_studies(&mut self, cx: &mut Context<Self>) {
        if !self.retry_automatic_study_removals() {
            return;
        }
        self.dispatch_deferred_runtime_studies(cx);
        let Ok(series) = self.current_runtime_series() else {
            return;
        };
        let stale = self
            .studies
            .active
            .iter()
            .filter(|state| {
                runtime_study_uses_current_chart(&state.persisted)
                    && state.resolved_chart_series.as_ref() != Some(&series)
                    && !self.studies.removing.contains(&state.study_id)
                    && !self.studies.automatic_removals.contains(&state.study_id)
                    && !self.studies.reinitializing.contains_key(&state.study_id)
            })
            .map(|state| state.study_id)
            .collect::<Vec<_>>();
        for study_id in stale {
            if let Err(message) =
                self.queue_runtime_study_reinitialization(study_id, series.clone())
            {
                self.indicator_message = Some(message);
                cx.notify();
            }
        }
    }

    fn apply_study_registered(
        &mut self,
        request_sequence: u64,
        study_id: StudyInstanceId,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.studies.pending.remove(&request_sequence) else {
            return;
        };
        let runtime_state = RuntimeStudyState {
            study_id,
            persisted: state.persisted.clone(),
            resolved_chart_series: state.resolved_chart_series,
        };
        if state.remove_on_registration {
            self.studies.active.push(runtime_state);
            self.studies.automatic_removals.insert(study_id);
            let _ = self.retry_automatic_study_removals();
            self.chart_persistence_dirty = true;
            cx.notify();
            return;
        }
        self.studies.active.push(runtime_state);
        if state.persist_on_registration {
            self.chart_persistence_dirty = true;
        }
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_study_visible(study_id.get(), state.persisted.visible) {
                    chart_cx.notify();
                }
            });
        }
        self.indicator_message = None;
        self.synchronize_runtime_studies(cx);
        cx.notify();
    }

    fn apply_study_registration_failed(
        &mut self,
        request_sequence: u64,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.studies.pending.remove(&request_sequence) else {
            return;
        };
        if state.persist_on_registration {
            self.chart_persistence_dirty = true;
        }
        if state.remove_on_registration {
            self.indicator_message = None;
            cx.notify();
            return;
        }
        if !state.persist_on_registration {
            let mut state = state;
            state.blocked = true;
            self.studies.deferred.insert(0, state);
        }
        self.indicator_message = Some(message);
        cx.notify();
    }

    fn apply_study_reinitialized(&mut self, study_id: StudyInstanceId, cx: &mut Context<Self>) {
        let Some(persisted_changed) = self.studies.complete_reinitialization(study_id) else {
            return;
        };
        if persisted_changed {
            self.chart_persistence_dirty = true;
        }
        if self
            .study_settings_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.study_id == study_id)
        {
            self.study_settings_dialog = None;
        }
        self.indicator_message = None;
        cx.notify();
    }

    fn apply_study_output(
        &self,
        snapshot: &aeris_market_runtime::MarketStudyOutputSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.studies.suppresses_output(snapshot.study_id) {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let visible = chart
            .read(cx)
            .study_visible(snapshot.study_id.get())
            .or_else(|| {
                self.studies
                    .active
                    .iter()
                    .find(|state| state.study_id == snapshot.study_id)
                    .map(|state| state.persisted.visible)
            });
        // Every study has host-owned style settings, so the legend always offers them.
        let settings_available = true;
        let line_width = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == snapshot.study_id)
            .map_or_else(
                || default_study_line_width(&snapshot.study_identifier),
                |state| persisted_study_line_width(&state.persisted),
            );
        chart.update(cx, |chart, chart_cx| {
            chart.set_study_line_width(snapshot.study_id.get(), line_width);
            match chart.install_study_output(
                snapshot.study_id.get(),
                snapshot.output_id.output_index,
                ChartStudyOutputDescriptor {
                    title: &snapshot.output.title,
                    legend_label: snapshot.output.legend_label.as_deref(),
                    plot: chart_study_plot(snapshot.output.plot),
                    pane: chart_study_pane(snapshot.output.pane),
                    scale: chart_study_scale(snapshot.output.scale),
                    settings_available,
                    threshold_region: chart_study_threshold(snapshot.output.threshold_region),
                    point_style: chart_study_point_style(snapshot.output.point_style),
                    input_requirements: chart_study_input_requirements(
                        snapshot.stream_requirements,
                    ),
                },
                snapshot.series.generation(),
                snapshot.series.timestamps(),
                snapshot.series.values(),
            ) {
                Ok(true) => {
                    if let Some(visible) = visible {
                        chart.set_study_visible(snapshot.study_id.get(), visible);
                    }
                    chart_cx.notify();
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!("Aeris study output could not be displayed: {error}");
                }
            }
        });
    }

    fn apply_study_removed(
        &mut self,
        removed: &aeris_market_runtime::MarketStudyRemoved,
        cx: &mut Context<Self>,
    ) {
        let study_ids = removed
            .study_ids
            .iter()
            .map(|study_id| study_id.get())
            .collect::<Vec<_>>();
        let removed_runtime = self.studies.remove_runtime_subtree(&removed.study_ids);
        if removed_runtime {
            self.chart_persistence_dirty = true;
        }
        if self
            .study_settings_dialog
            .as_ref()
            .is_some_and(|dialog| removed.study_ids.contains(&dialog.study_id))
        {
            self.study_settings_dialog = None;
        }
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_study_outputs(&study_ids) {
                    chart_cx.notify();
                }
            });
        }
    }

    fn apply_study_invalidated(
        &mut self,
        invalidated: &aeris_market_runtime::MarketStudyOutputsInvalidated,
        cx: &mut Context<Self>,
    ) {
        self.studies
            .invalidate_study_outputs(&invalidated.study_ids);
        let Some(chart) = &self.chart else {
            return;
        };
        let study_ids = invalidated
            .study_ids
            .iter()
            .map(|study_id| study_id.get())
            .collect::<Vec<_>>();
        chart.update(cx, |chart, chart_cx| {
            if chart.remove_study_outputs(&study_ids) {
                chart_cx.notify();
            }
        });
    }

    fn apply_market_state_message(
        &mut self,
        state: ChartState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        // The pending-switch tracker is provider-neutral: both engine
        // providers resolve selections through the same marker flow.
        let engine_provider = super::provider_presentation(self.provider).is_some();
        if self.stale_settled_state_during_engine_switch(state, engine_provider) {
            return;
        }
        if state == ChartState::Error && engine_provider {
            let swapping = self.rithmic_switch.is_swapping();
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_pending_interval = None;
            self.rithmic_pending_product = None;
            self.rithmic_pending_sequence = None;
            self.market_state.symbol_selection_pending = false;
            if swapping {
                self.restore_rithmic_selection_after_failure(&message, cx);
            } else {
                self.rithmic_previous_selection = None;
            }
        } else if engine_provider && state == ChartState::Ready {
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_previous_selection = None;
            self.market_state.symbol_selection_pending = false;
            self.symbol_message = self.product.as_ref().map_or_else(
                || format!("{} market ready", terminal_provider_display(self.provider)),
                |product| super::provider_ready_message(self.provider, product),
            );
        } else if engine_provider && state == ChartState::AwaitingData {
            // The selection itself succeeded. A chart still showing the previous
            // series stays covered (`Swapping`) until the replacement's first
            // snapshot; a replacement already on screen is simply this series.
            if self.rithmic_switch == RithmicSwitchState::Initializing {
                self.rithmic_switch = RithmicSwitchState::Idle;
                self.rithmic_previous_selection = None;
            }
            self.market_state.symbol_selection_pending = false;
        }
        self.set_chart_state(state, message, cx);
    }

    fn apply_market_connection_message(
        &mut self,
        state: FeedConnectionState,
        message: String,
        transport_rtt_nanos: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        self.provider_transport_rtt_nanos = if state == FeedConnectionState::Streaming {
            transport_rtt_nanos
        } else {
            None
        };
        self.apply_connection_state(state, message, cx);
    }

    fn stale_settled_state_during_engine_switch(
        &self,
        state: ChartState,
        engine_provider: bool,
    ) -> bool {
        // The previous series can report one last Ready after the catalog
        // response but before the worker processes the new EngineSelect
        // command. Keep the switch pending until its marker/snapshot lands.
        // The same holds for a settled no-data state queued before the marker.
        engine_provider
            && match state {
                ChartState::Ready => !ready_state_can_complete_switch(self.rithmic_switch),
                ChartState::AwaitingData => self.rithmic_switch.is_pending(),
                _ => false,
            }
    }

    fn apply_provider_catalog_event(
        &mut self,
        event: ProviderCatalogEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_catalog_event(event, cx);
    }

    fn apply_catalog_results(
        &mut self,
        generation: u64,
        instruments: Vec<ProviderInstrumentSummary>,
    ) -> Option<usize> {
        let generation = usize_generation(generation)?;
        let count = instruments.len();
        self.symbol_browser
            .apply_results(generation, instruments)
            .then_some(count)
    }

    fn confirm_catalog_selection(&mut self, command_generation: u64) -> bool {
        usize_generation(command_generation)
            .is_some_and(|generation| self.symbol_browser.confirm_selection(generation))
    }

    fn consume_catalog_search_authorization(&mut self) {
        if let Some(search_generation) = self
            .symbol_browser
            .selected()
            .map(|selection| selection.search_generation)
        {
            self.symbol_browser
                .consume_completed_search(search_generation);
        }
    }

    /// Commits a Rithmic switch's identity without touching the chart.
    ///
    /// The marker only says "everything after this belongs to the new
    /// selection". The chart the trader is looking at is left on screen — still
    /// its own series, still correct — under a loading notice, and is replaced
    /// in `apply_publication` when the replacement's covering snapshot arrives.
    /// Dropping it here is what produced the blank surface on every switch.
    fn apply_rithmic_switch_marker(&mut self, sequence: u64, cx: &mut Context<Self>) {
        if super::provider_presentation(self.provider).is_none()
            || !self.rithmic_switch.is_pending()
            || self.rithmic_pending_sequence != Some(sequence)
        {
            return;
        }
        self.rithmic_previous_selection = Some((self.product.clone(), self.interval));
        // Capture presentation against the chart's current instrument before
        // mutating the pending selection. Timeframe replacement can reuse
        // drawings; a symbol replacement must not carry price-anchored drawings
        // onto another instrument.
        self.retain_chart_presentation(cx);
        if let Some(interval) = self.rithmic_pending_interval.take() {
            self.interval = interval;
        }
        if let Some(product) = self.rithmic_pending_product.take() {
            let provider = terminal_provider_from_id(&product.provider);
            if provider != self.provider {
                self.provider = provider;
                self.symbol_message = initial_symbol_message(provider);
            }
            self.product = Some(product);
            // Price levels belong to one instrument: a product switch drops
            // the old book back to loading instead of showing BTC levels
            // under an ETH selection. Interval-only switches keep the book.
            self.order_book.update(cx, |order_book, order_book_cx| {
                order_book.clear(order_book_cx);
            });
            self.trade_tape = None;
            self.trade_sweeps = Arc::from([]);
        }
        self.rithmic_pending_sequence = None;
        self.rithmic_switch = if self.chart.is_some() {
            RithmicSwitchState::Swapping
        } else {
            RithmicSwitchState::Idle
        };
        self.restored_viewport = None;
        self.last_persisted_viewport = None;
        self.chart_state = ChartState::Loading;
        cx.notify();
    }

    /// Restores the selection a failed switch was replacing.
    ///
    /// The chart on screen is still the previous series, so restoring means
    /// re-stating its demand and reporting an actionable error over it — never
    /// leaving the trader on a surface with no data and no way back.
    fn restore_rithmic_selection_after_failure(&mut self, detail: &str, cx: &mut Context<Self>) {
        let Some((product, interval)) = self.rithmic_previous_selection.take() else {
            return;
        };
        self.product.clone_from(&product);
        self.interval = interval;
        self.rithmic_pending_interval = None;
        self.rithmic_pending_product = None;
        self.rithmic_pending_sequence = None;
        self.rithmic_switch = RithmicSwitchState::Idle;
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.clear(order_book_cx);
        });
        self.trade_tape = None;
        self.trade_sweeps = Arc::from([]);
        let restored = product
            .and_then(|product| self.market_worker.try_select_engine(product, interval).ok());
        if let Some(sequence) = restored {
            self.rithmic_pending_sequence = Some(sequence);
            self.rithmic_pending_interval = Some(interval);
            self.rithmic_switch = RithmicSwitchState::Pending;
        }
        self.series_message = format!("{detail} — showing {}", interval.label());
        cx.notify();
    }

    pub(super) fn poll_market_worker(&mut self, cx: &mut Context<Self>) -> usize {
        let context_changed = self.context_view.as_ref().is_some_and(|view| {
            let snapshot = view.snapshot();
            if snapshot.revision == self.context_snapshot.revision {
                return false;
            }
            self.context_snapshot = snapshot;
            true
        });
        if context_changed {
            cx.notify();
        }
        let chart_was_missing = self.chart.is_none();
        let (messages, disconnected) = self
            .market_worker
            .drain_messages_up_to(MARKET_MESSAGES_PER_FRAME);
        let applied = messages.len();
        let chart_update_received = messages
            .iter()
            .any(|message| matches!(message, MarketWorkerMessage::Update(_)));
        for message in messages {
            self.apply_market_worker_message(message, cx);
        }
        if super::provider_presentation(self.provider).is_some()
            && disconnected
            && !matches!(self.connection_state, Some(FeedConnectionState::Stopped))
        {
            let display = terminal_provider_display(self.provider);
            let fallback = format!("{display} market worker stopped");
            let detail =
                stopped_worker_chart_detail(self.chart_state, &self.chart_state_message, &fallback);
            self.apply_connection_state(FeedConnectionState::Stopped, fallback, cx);
            if self.chart_state == ChartState::Error && self.chart_state_message != detail {
                self.chart_state_message = detail;
                cx.notify();
            }
        } else if disconnected && self.chart_state != ChartState::Error {
            let message = format!(
                "{} market worker stopped",
                terminal_provider_display(self.provider)
            );
            self.connection_state = Some(FeedConnectionState::Stopped);
            self.connection_message = Some(message.clone());
            self.set_chart_state(ChartState::Error, message, cx);
        }
        self.dispatch_recovery(cx);
        self.dispatch_retained_symbol_search(cx);
        self.synchronize_runtime_studies(cx);

        let status = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| {
                let metrics = chart.read(cx).replay_bridge_metrics();
                let reconciled =
                    reconciled_bridge_state(self.chart_state, metrics.recovery_pending);
                if reconciled != self.chart_state {
                    self.chart_state = reconciled;
                    self.chart_state_message =
                        "chart validation requires a correlated covering snapshot".to_string();
                    cx.notify();
                }
                bridge_status(metrics)
            },
        );
        if self.bridge_label != status {
            self.bridge_label = status;
            cx.notify();
        }
        let loading = self.chart_state == ChartState::Loading;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_asset_loading(loading) {
                    chart_cx.notify();
                }
            });
        }
        if applied > 0 {
            if chart_was_missing && self.chart.is_some() {
                cx.notify();
            } else if chart_update_received && let Some(chart) = &self.chart {
                chart.update(cx, |_, chart_cx| chart_cx.notify());
            }
        }
        applied + usize::from(disconnected) + usize::from(context_changed)
    }

    fn apply_connection_state(
        &mut self,
        state: FeedConnectionState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let state = stabilized_connection_state(self.connection_state, state);
        if state != FeedConnectionState::Streaming {
            self.provider_transport_rtt_nanos = None;
        }
        let presentation = ProviderConnectionPresentation::from_connection(state);
        let retained_market_data = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        if let Some(chart_state) = presentation.chart_state(retained_market_data) {
            self.chart_state = chart_state;
            self.chart_state_message.clone_from(&message);
        }
        self.connection_state = Some(state);
        // Depth follows the same honesty rule as the empty panel: a fresh
        // demand restarts from loading, and only a concrete stop marks the
        // book unavailable. Provider recovery itself is owned by the market runtime.
        match state {
            FeedConnectionState::Disconnected => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        aeris_terminal_ui::OrderBookConnectionState::Offline,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Recovering => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        aeris_terminal_ui::OrderBookConnectionState::Recovering,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Streaming => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        aeris_terminal_ui::OrderBookConnectionState::Online,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Stopped => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.mark_unavailable(order_book_cx);
                });
            }
        }
        let autoload_catalog = should_autoload_rithmic_catalog(
            state,
            &message,
            self.market_state.rithmic_autoload_started,
        );
        self.connection_message = Some(stable_connection_message(state, message));
        if autoload_catalog {
            self.market_state.rithmic_autoload_started = true;
            self.market_state.rithmic_autoload_selection_pending = true;
            let _ = self.search_symbol_query(default_listing_query(self.provider), cx);
        }
        cx.notify();
    }

    pub(super) fn apply_theme(&mut self, theme: &AerisTheme, cx: &mut Context<Self>) {
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.set_theme(*theme, order_book_cx);
        });
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(aeris_chart_theme(theme.mode));
                chart_cx.notify();
            });
        }
        self.theme = *theme;
        cx.notify();
    }

    pub(super) fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
        cx: &mut Context<Self>,
    ) {
        self.chart_chrome.indicator_name_labels_visible = names;
        self.chart_chrome.indicator_value_labels_visible = values;
        self.chart_chrome.indicator_price_lines_visible = price_lines;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.apply_indicator_chrome_preferences(names, values, price_lines);
                chart_cx.notify();
            });
        }
    }

    fn apply_chart_chrome_to_chart(&self, chart: &Entity<AerisChartView>, cx: &mut Context<Self>) {
        chart.update(cx, |chart, _| {
            chart.apply_indicator_chrome_preferences(
                self.chart_chrome.indicator_name_labels_visible,
                self.chart_chrome.indicator_value_labels_visible,
                self.chart_chrome.indicator_price_lines_visible,
            );
            chart.set_chart_type(self.chart_chrome.chart_type);
            let _ = chart.apply_price_axis_menu_action(
                0,
                false,
                PriceAxisMenuAction::SetPrecision(self.retained_chart_presentation.price_precision),
            );
        });
    }

    fn retain_chart_presentation(&mut self, cx: &App) {
        if let Some(chart) = &self.chart
            && chart.read(cx).has_market_data()
        {
            for state in &mut self.studies.active {
                if let Some(visible) = chart.read(cx).study_visible(state.study_id.get()) {
                    state.persisted.visible = visible;
                }
            }
            self.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
            self.retained_chart_presentation.price_precision =
                chart.read(cx).selected_price_precision();
            self.retained_chart_presentation.chart_state = self.workspace_chart_state(cx);
            self.retained_chart_presentation.instrument_id = self
                .product
                .as_ref()
                .map(|product| product.instrument_id.clone());
        }
    }

    fn apply_retained_chart_state_to_chart(
        &self,
        chart: &Entity<AerisChartView>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &self.retained_chart_presentation.chart_state {
            let same_instrument = self
                .product
                .as_ref()
                .map(|product| product.instrument_id.as_str())
                == self.retained_chart_presentation.instrument_id.as_deref();
            Self::apply_restored_chart_state(chart, state, same_instrument, cx);
            return;
        }
        self.apply_retained_indicators_to_chart(chart, cx);
    }

    fn apply_retained_indicators_to_chart(
        &self,
        chart: &Entity<AerisChartView>,
        cx: &mut Context<Self>,
    ) {
        if self.retained_chart_presentation.indicators.is_empty() {
            return;
        }
        let states = self.retained_chart_presentation.indicators.clone();
        let result = chart.update(cx, |chart, _| chart.restore_indicator_states(&states));
        if let Err(error) = result {
            eprintln!("Aeris chart indicators could not be restored: {error}");
        }
    }

    pub(super) fn chart_type(&self, cx: &App) -> ChartType {
        self.chart
            .as_ref()
            .map_or(self.chart_chrome.chart_type, |chart| {
                chart.read(cx).chart_type()
            })
    }

    pub(super) fn chart_appearance(&self, cx: &App) -> Option<ChartAppearanceSettings> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).appearance_settings())
    }

    pub(super) fn chart_crosshair_mode(&self, cx: &App) -> Option<u8> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).crosshair_mode())
    }

    pub(super) fn chart_order_flow_settings(&self, cx: &App) -> Option<OrderFlowSettings> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).order_flow_settings())
    }

    pub(super) const fn chart_trading_visibility(&self) -> ChartTradingVisibilitySettings {
        self.chart_trading_visibility
    }

    pub(super) fn set_chart_trading_visibility(
        &mut self,
        visibility: ChartTradingVisibilitySettings,
        cx: &mut Context<Self>,
    ) {
        if self.chart_trading_visibility == visibility {
            return;
        }
        self.chart_trading_visibility = visibility;
        if let Some(chart) = &self.chart {
            let current = chart.read(cx).trading_snapshot();
            let filtered = workspace_layout::apply_chart_trading_visibility(current, visibility);
            chart.update(cx, |chart, chart_cx| {
                if chart.set_trading_snapshot(filtered).is_ok() {
                    chart_cx.notify();
                }
            });
        }
        // Enabling either layer needs the next authoritative runtime snapshot to restore any
        // presentation objects that were intentionally filtered out while hidden.
        self.trading_pnl.next_refresh = std::time::Instant::now();
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    /// Adds a tape-derived order-flow indicator through the order-flow settings owner.
    pub(super) fn add_order_flow_study(
        &mut self,
        study: OrderFlowMenuStudy,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut settings) = self.chart_order_flow_settings(cx) else {
            return false;
        };
        match study {
            OrderFlowMenuStudy::CumulativeDelta => settings.show_cumulative_delta = true,
            OrderFlowMenuStudy::Delta => settings.show_delta_histogram = true,
            OrderFlowMenuStudy::BigTrades => {
                settings.big_trades = Some(settings.big_trades.unwrap_or_default());
            }
        }
        self.set_chart_order_flow_settings(settings, cx);
        true
    }

    pub(super) fn set_chart_order_flow_settings(
        &mut self,
        settings: OrderFlowSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        match chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_order_flow_settings(settings)?;
            if changed {
                chart_cx.notify();
            }
            Ok::<_, String>(changed)
        }) {
            Ok(true) => {
                self.chart_persistence_dirty = true;
                if let Some(snapshot) = self.trade_tape.clone() {
                    self.apply_trade_tape_to_chart(&snapshot, cx);
                }
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => self.indicator_message = Some(error),
        }
    }

    pub(super) fn cycle_time_sales_side_filter(&mut self, cx: &mut Context<Self>) {
        self.time_sales_filter.side = self.time_sales_filter.side.next();
        cx.notify();
    }

    pub(super) fn cycle_time_sales_size_filter(&mut self, cx: &mut Context<Self>) {
        self.time_sales_filter.minimum_quantity = match self.time_sales_filter.minimum_quantity {
            value if value < 1.0 => 1.0,
            value if value < 10.0 => 10.0,
            value if value < 100.0 => 100.0,
            _ => 0.0,
        };
        cx.notify();
    }

    pub(super) fn cycle_time_sales_price_filter(&mut self, cx: &mut Context<Self>) {
        self.time_sales_filter.price_range_ticks = match self.time_sales_filter.price_range_ticks {
            None => Some(10),
            Some(10) => Some(50),
            Some(_) => None,
        };
        cx.notify();
    }

    pub(super) fn reset_time_sales_filter(&mut self, cx: &mut Context<Self>) {
        if self.time_sales_filter.reset() {
            cx.notify();
        }
    }

    pub(super) fn suspend_chart_pointer(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, _| chart.suspend_pointer_interaction());
        }
    }

    pub(super) fn resume_chart_pointer(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, _| chart.resume_pointer_interaction());
        }
    }

    pub(super) fn set_chart_appearance(
        &mut self,
        appearance: &ChartAppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_appearance_settings(appearance);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_chart_canvas_appearance(
        &mut self,
        appearance: &ChartAppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_canvas_appearance_settings(appearance);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_chart_series_appearance(
        &mut self,
        appearance: &ChartAppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_series_appearance_settings(appearance);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn reset_chart_appearance(&mut self, cx: &mut Context<Self>) {
        let Some(chart) = &self.chart else {
            return;
        };
        chart.update(cx, |chart, chart_cx| {
            chart.reset_appearance_settings();
            chart_cx.notify();
        });
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn set_chart_crosshair_mode(&mut self, mode: u8, cx: &mut Context<Self>) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_crosshair_mode(mode);
            if changed {
                chart_cx.notify();
            }
            changed
        }) {
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    fn apply_trade_tape_to_chart(
        &mut self,
        snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot,
        cx: &mut Context<Self>,
    ) {
        const SWEEP_CLASSIFICATION_WINDOW: usize = 512;
        let (Some(chart), Some(product), Some(trades)) = (
            self.chart.as_ref(),
            self.product.as_ref(),
            chart_order_flow_trades(snapshot),
        ) else {
            return;
        };
        let recent_start = trades.len().saturating_sub(SWEEP_CLASSIFICATION_WINDOW);
        self.trade_sweeps = classify_order_flow_sweeps(&trades[recent_start..]).into();
        let price_divisor = 10_f64.powi(i32::from(snapshot.price_scale));
        // Footprint rows are keyed by the instrument's price increment. Without a
        // provider-published increment the chart stays on candles; none is guessed.
        let Some(tick_size) = product
            .price_increment
            .and_then(|increment| {
                num_traits::ToPrimitive::to_f64(&increment)
                    .map(|increment| increment / price_divisor)
            })
            .filter(|tick| tick.is_finite() && *tick > 0.0)
        else {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_order_flow_trades();
                chart_cx.notify();
            });
            self.indicator_message = Some(format!(
                "Footprint is unavailable: {} has no published price increment",
                product.display_symbol
            ));
            return;
        };
        let identity = product.instrument_id.clone();
        let aggregation = order_flow_aggregation(self.interval);
        let result = chart.update(cx, |chart, chart_cx| {
            if self
                .trade_tape
                .as_ref()
                .is_some_and(|prior| prior.rewrite_generation != snapshot.rewrite_generation)
            {
                chart.invalidate_order_flow_prefix();
            }
            let result = chart.apply_order_flow_trades(
                &identity,
                snapshot.provider_generation,
                aggregation,
                tick_size,
                &trades,
            );
            if result.is_ok() {
                chart_cx.notify();
            }
            result
        });
        if let Err(error) = result {
            self.indicator_message = Some(format!(
                "Order-flow chart could not apply the current trade tape: {error}"
            ));
        }
    }

    pub(super) fn set_chart_type(&mut self, chart_type: ChartType, cx: &mut Context<Self>) {
        self.chart_chrome.chart_type = chart_type;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_chart_type(chart_type);
                chart_cx.notify();
            });
        }
        if let Some(snapshot) = self.trade_tape.clone() {
            self.apply_trade_tape_to_chart(&snapshot, cx);
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }

    /// Queues the durable chrome preferences off the UI thread; the save worker coalesces.
    fn save_chart_chrome_preferences(&self, cx: &mut Context<Self>) {
        let preferences = self.chart_chrome;
        match chart_chrome::request_chart_chrome_preferences_save(preferences) {
            Ok(true) => cx
                .background_executor()
                .spawn(async move {
                    if let Err(error) = chart_chrome::run_chart_chrome_preferences_save_worker() {
                        eprintln!("Aeris chart chrome could not be saved: {error}");
                    }
                })
                .detach(),
            Ok(false) => {}
            Err(error) => {
                eprintln!("Aeris chart chrome could not be saved: {error}");
            }
        }
    }

    fn refresh_default_instrument_listing(&mut self, cx: &mut Context<Self>) {
        if !instrument_listing_refresh_needed(
            &self.symbol_browser,
            self.market_state.symbol_selection_pending,
        ) {
            return;
        }
        let _ = self.search_symbol_query(default_listing_query(self.symbol_provider), cx);
    }

    fn search_symbol_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        if self.symbol_browser.search_pending() {
            match self.symbol_browser.retain_latest_search(query) {
                Ok(already_dispatched) => {
                    if !already_dispatched {
                        let display = terminal_provider_display(self.symbol_provider);
                        self.symbol_message =
                            format!("Waiting to search the latest {display} query");
                    }
                    cx.notify();
                    return already_dispatched;
                }
                Err(message) => {
                    self.symbol_message = message.to_string();
                    cx.notify();
                }
            }
            return false;
        }
        let request = match self.symbol_browser.begin_search(query) {
            Ok(request) => request,
            Err(message) => {
                self.symbol_message = message.to_string();
                cx.notify();
                return false;
            }
        };
        self.dispatch_symbol_search(request, cx)
    }

    fn dispatch_symbol_search(
        &mut self,
        request: rithmic_shell::RithmicSymbolSearchRequest,
        cx: &mut Context<Self>,
    ) -> bool {
        let provider = terminal_provider_id(self.symbol_provider);
        let retained_query = request.query.clone();
        let request_id = request.request_id;
        let search = SearchProviderInstruments {
            consumer_id: 0,
            search_generation: u64::try_from(request_id.get()).unwrap_or(u64::MAX),
            provider: provider.to_string(),
            query: request.query,
            maximum_results: u32::try_from(self.symbol_browser.maximum_results())
                .unwrap_or(u32::MAX),
            categories: self.chart_chrome.symbol_search_categories,
        };
        let dispatched = if self.market_worker.try_search_provider(search).is_ok() {
            self.symbol_message = super::provider_presentation(self.symbol_provider).map_or_else(
                || "Searching provider markets".to_string(),
                |descriptor| descriptor.search_hint.to_string(),
            );
            true
        } else {
            self.symbol_browser.reject_search(request_id);
            let _ = self.symbol_browser.retain_latest_search(&retained_query);
            self.symbol_message = "Symbol search is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn dispatch_retained_symbol_search(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(request) = self.symbol_browser.begin_retained_search() {
            return self.dispatch_symbol_search(request, cx);
        }
        false
    }

    pub(super) fn search_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(input) = &self.symbol_input else {
            return false;
        };
        let query = input.read(cx).value().to_string();
        self.search_symbol_query(&query, cx)
    }

    pub(super) fn search_mnemonic_symbol(
        &mut self,
        symbol: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(input) = self.symbol_input.clone() else {
            self.symbol_message = "Symbol search is unavailable".to_string();
            cx.notify();
            return false;
        };
        input.update(cx, |input, input_cx| {
            input.set_value(&symbol, window, input_cx);
        });
        self.pending_mnemonic_symbol = Some(symbol);
        if self.search_symbol_input(cx) {
            true
        } else {
            self.pending_mnemonic_symbol = None;
            false
        }
    }

    pub(super) fn submit_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let entries = self.instrument_entries(cx);
        match symbol_submit_decision(self.symbol_provider, entries.len(), self.chrome_selection) {
            SymbolSubmitDecision::Select(index) => entries.get(index).is_some_and(|entry| {
                self.select_instrument(entry.selection, self.symbol_selection_target, cx)
            }),
            SymbolSubmitDecision::Search => {
                self.search_symbol_input(cx);
                false
            }
        }
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(selection) = self.symbol_browser.select(index) else {
            return false;
        };
        let entitlement_id = format!(
            "rithmic-test:{}:{}",
            selection.instrument.exchange, selection.instrument.symbol
        );
        let request = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: u64::try_from(selection.generation.get()).unwrap_or(u64::MAX),
            search_generation: u64::try_from(selection.search_generation.get()).unwrap_or(u64::MAX),
            provider: "rithmic".to_string(),
            symbol: selection.instrument.symbol.clone(),
            exchange: selection.instrument.exchange.clone(),
            entitlement_id,
        };
        let dispatched = if self.market_worker.try_select_provider(request).is_ok() {
            self.market_state.symbol_selection_pending = true;
            self.order_book
                .update(cx, aeris_terminal_ui::ReadOnlyOrderBookView::clear);
            self.symbol_message = format!(
                "Selecting {} · {}",
                selection.instrument.display_symbol, selection.instrument.exchange
            );
            true
        } else {
            self.symbol_browser.reject_selection(selection.generation);
            self.symbol_message = "Symbol selection is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn apply_catalog_event(&mut self, event: ProviderCatalogEvent, cx: &mut Context<Self>) {
        if provider_catalog_event_provider(&event) != terminal_provider_id(self.symbol_provider) {
            return;
        }
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                self.apply_search_completed(result, cx);
            }
            ProviderCatalogEvent::SearchPreview(result) => {
                if let Some(generation) = usize_generation(result.search_generation) {
                    let count = result.instruments.len();
                    if self
                        .symbol_browser
                        .apply_preview_results(generation, result.instruments)
                    {
                        self.symbol_message = format!("{count} matching markets");
                    }
                }
            }
            ProviderCatalogEvent::SelectionInstalled {
                command_generation,
                instrument,
            } => {
                // Engine providers resolve selections through the same
                // switch flow: the pending product replaces the chart only
                // when its covering snapshot arrives.
                if self.pending_symbol_selection_target == Some(SymbolSelectionTarget::Watchlist) {
                    let Some(_selection) = usize_generation(command_generation)
                        .and_then(|generation| self.symbol_browser.resolve_selection(generation))
                    else {
                        return;
                    };
                    self.pending_watchlist_instrument = Some(instrument);
                    self.pending_symbol_selection_target = None;
                    if self.chrome_overlay != Some(ChromeOverlay::Instrument)
                        || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
                    {
                        self.symbol_selection_target = SymbolSelectionTarget::Chart;
                    }
                    self.market_state.symbol_selection_pending = false;
                    self.symbol_message = "Watchlist symbol resolved".to_string();
                    cx.notify();
                    return;
                }
                if !self.confirm_catalog_selection(command_generation) {
                    return;
                }
                self.pending_symbol_selection_target = None;
                self.consume_catalog_search_authorization();
                let display = terminal_provider_display(self.symbol_provider);
                let interval = self.rithmic_pending_interval.unwrap_or(self.interval);
                let Ok(sequence) = self
                    .market_worker
                    .try_select_engine(instrument.clone(), interval)
                else {
                    self.market_state.symbol_selection_pending = false;
                    self.symbol_message = format!("{display} market history could not start");
                    return;
                };
                self.pending_linked_instrument = Some(instrument.clone());
                self.rithmic_pending_product = Some(instrument);
                self.rithmic_pending_interval = Some(interval);
                self.rithmic_pending_sequence = Some(sequence);
                self.rithmic_switch = RithmicSwitchState::Pending;
                self.chart_state = ChartState::Loading;
                self.chart_state_message = format!("Loading {} market history", interval.label());
                self.symbol_message = format!("Loading the selected {display} market");
            }
            ProviderCatalogEvent::StartupInstrumentResolved(instrument) => {
                let same_instrument = self.product.as_ref().is_some_and(|product| {
                    product.provider == instrument.provider
                        && product.instrument_id == instrument.instrument_id
                });
                if same_instrument {
                    self.product = Some(instrument);
                    self.chart_persistence_dirty = true;
                }
            }
            ProviderCatalogEvent::CommandRejected { rejection, command } => {
                self.apply_catalog_rejection(&rejection, command, cx);
            }
        }
        cx.notify();
    }

    fn apply_search_completed(
        &mut self,
        result: ProviderInstrumentSearchResult,
        cx: &mut Context<Self>,
    ) {
        let Some(count) = self.apply_catalog_results(result.search_generation, result.instruments)
        else {
            return;
        };
        let display = terminal_provider_display(self.symbol_provider);
        self.symbol_message = format!("{count} {display} markets · search to find more");
        if self.symbol_browser.has_retained_search() {
            self.dispatch_retained_symbol_search(cx);
            cx.notify();
            return;
        }
        if let Some(symbol) = self.pending_mnemonic_symbol.take() {
            let index = self.symbol_browser.results().iter().position(|instrument| {
                instrument.symbol.eq_ignore_ascii_case(&symbol)
                    || instrument.display_symbol.eq_ignore_ascii_case(&symbol)
            });
            if let Some(index) = index {
                self.chrome_selection = index;
                self.select_instrument(
                    InstrumentMenuSelection(index),
                    SymbolSelectionTarget::Chart,
                    cx,
                );
                return;
            }
            self.symbol_message = format!("No exact market matched {symbol}");
        }
        if self.symbol_provider == TerminalProvider::Rithmic
            && self.market_state.rithmic_autoload_selection_pending
            && self.symbol_browser.selected().is_none()
            && let Some(index) = default_rithmic_contract_index(self.symbol_browser.results())
        {
            self.market_state.rithmic_autoload_selection_pending = false;
            self.select_rithmic_symbol(index, cx);
        }
        self.dispatch_retained_symbol_search(cx);
        if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
            self.sync_instrument_menu_keyboard(cx);
        }
    }

    fn apply_catalog_rejection(
        &mut self,
        rejection: &ProviderCatalogRejected,
        command: ProviderCatalogCommand,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = usize_generation(rejection.command_generation) else {
            return;
        };
        let selection = command == ProviderCatalogCommand::Selection;
        let rejected = if selection {
            self.symbol_browser.reject_selection(generation)
        } else {
            self.symbol_browser.reject_search(generation)
        };
        if !rejected {
            return;
        }
        if selection {
            self.pending_symbol_selection_target = None;
            if self.chrome_overlay != Some(ChromeOverlay::Instrument)
                || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
            {
                self.symbol_selection_target = SymbolSelectionTarget::Chart;
            }
        }
        self.market_state.symbol_selection_pending = false;
        let reason = rejection.reason;
        self.symbol_message = catalog_rejection_message(reason, command, self.provider);
        self.dispatch_retained_symbol_search(cx);
    }

    /// Whether a market is selected. The header enables the market panels on this and
    /// `toggle_side_panel` opens them on it, so the two cannot drift apart.
    pub(super) fn has_market_selection(&self) -> bool {
        self.symbol_browser.selected().is_some() || self.product.is_some()
    }

    pub(super) fn toggle_side_panel(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        if panel.requires_market() && !self.has_market_selection() {
            return;
        }
        let visible = !self.side_panels.contains(panel);
        self.set_side_panel_visible(panel, visible, cx);
    }

    pub(super) fn toggle_order_book(&mut self, cx: &mut Context<Self>) {
        self.toggle_side_panel(SidePanel::OrderBook, cx);
    }

    pub(super) fn toggle_time_sales(&mut self, cx: &mut Context<Self>) {
        self.toggle_side_panel(SidePanel::TimeSales, cx);
    }

    pub(super) fn toggle_watchlist(&mut self, cx: &mut Context<Self>) {
        self.toggle_side_panel(SidePanel::Watchlist, cx);
    }

    pub(super) fn set_side_panel_visible(
        &mut self,
        panel: SidePanel,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        if self.side_panels.contains(panel) == visible {
            return;
        }
        self.side_panels.set(panel, visible);
        if panel == SidePanel::OrderBook {
            if visible {
                self.order_book
                    .update(cx, aeris_terminal_ui::ReadOnlyOrderBookView::clear);
            } else {
                self.menu_state.order_book_column_open = false;
            }
            let _ = self.market_worker.try_set_order_book_visible(visible);
        }
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    /// Applies a whole docked-panel layout, as when the active pane hands its panels over.
    pub(super) fn apply_side_panels(
        &mut self,
        panels: SidePanelVisibility,
        cx: &mut Context<Self>,
    ) {
        for panel in SidePanel::ALL {
            self.set_side_panel_visible(panel, panels.contains(panel), cx);
        }
    }

    pub(super) fn toggle_context_panel(&mut self, cx: &mut Context<Self>) {
        self.set_context_panel_visible(!self.context_panel_visible, cx);
    }

    pub(super) fn set_context_panel_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.context_panel_visible == visible {
            return;
        }
        self.context_panel_visible = visible;
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn set_context_panel_height(&mut self, height: f32, cx: &mut Context<Self>) {
        let height = clamped_context_panel_height(height);
        if (height - self.context_panel_height).abs() > f32::EPSILON {
            self.context_panel_height = height;
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_context_panel_tab(&mut self, tab: ContextPanelTab, cx: &mut Context<Self>) {
        if self.context_panel_tab == tab {
            return;
        }
        self.context_panel_tab = tab;
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn toggle_context_credential_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_credential_dialog.take().is_none() {
            let inputs = [
                ContextSource::Eia,
                ContextSource::Usda,
                ContextSource::UsdaFas,
                ContextSource::Fred,
            ]
            .into_iter()
            .map(|source| {
                let input = cx.new(|input_cx| {
                    InputState::new(window, input_cx)
                        .placeholder("Enter a new key")
                        .masked(true)
                });
                (source, input)
            })
            .collect();
            self.context_credential_dialog = Some(ContextCredentialDialogState { inputs });
            self.context_credential_message = None;
        }
        cx.notify();
    }

    pub(super) fn save_context_api_keys(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.context_credential_dialog.take() else {
            return;
        };
        let entries = dialog
            .inputs
            .into_iter()
            .filter_map(|(source, input)| {
                let value = input.read(cx).value().to_string();
                (!value.trim().is_empty()).then_some((source, value))
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            self.context_credential_message =
                Some("No keys were entered; existing keys are unchanged.".to_string());
            cx.notify();
            return;
        }
        let Some(service) = self.lifecycle.context_service() else {
            self.context_credential_message = Some("Context runtime is unavailable.".to_string());
            cx.notify();
            return;
        };
        self.context_credential_message =
            Some("Saving keys to the native credential vault…".to_string());
        let save = cx.background_executor().spawn(async move {
            for (source, value) in entries {
                service.store_api_key(source, value)?;
            }
            Ok::<_, String>(())
        });
        cx.spawn(async move |surface, cx| {
            let result = save.await;
            let _ = surface.update(cx, |surface, surface_cx| {
                surface.context_credential_message = Some(match result {
                    Ok(()) => "Keys saved; source refreshes were scheduled.".to_string(),
                    Err(error) => format!("Keys could not be saved: {error}"),
                });
                surface_cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn cycle_chart_link_group(&mut self, cx: &mut Context<Self>) {
        self.chart_link_group = (self.chart_link_group + 1) % 5;
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    pub(super) fn take_chart_sync_events(
        &mut self,
    ) -> Vec<aeris_chart_integration::ChartSyncEvent> {
        self.pending_chart_sync_events.drain(..).collect()
    }

    pub(super) fn take_linked_instrument(&mut self) -> Option<InstallProviderInstrument> {
        self.pending_linked_instrument.take()
    }

    pub(super) fn apply_linked_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) -> bool {
        let selected = self.select_installed_instrument(instrument, cx);
        // A coordinator-applied selection must never bounce back through the group.
        self.pending_linked_instrument = None;
        selected
    }

    pub(super) fn apply_linked_chart_event(
        &mut self,
        kind: &aeris_chart_integration::ChartSyncEventKind,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chart) = self.chart.as_ref() else {
            return false;
        };
        let allowed = match kind {
            aeris_chart_integration::ChartSyncEventKind::Crosshair { .. }
            | aeris_chart_integration::ChartSyncEventKind::ClearCrosshair => {
                self.chart_link_flags & 0b01 != 0
            }
            aeris_chart_integration::ChartSyncEventKind::VisibleTimeRange { .. } => {
                self.chart_link_flags & 0b10 != 0
            }
        };
        if !allowed {
            return false;
        }
        let changed = chart.update(cx, |chart, _| chart.apply_external_sync_event(kind));
        if changed {
            cx.notify();
        }
        changed
    }

    pub(super) fn open_watchlist_symbol_menu_at(
        &mut self,
        trigger_position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chrome_overlay_at(ChromeOverlay::Instrument, trigger_position, window, cx);
        self.symbol_selection_target = SymbolSelectionTarget::Watchlist;
    }

    pub(super) fn toggle_order_book_column_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_state.order_book_column_open = !self.menu_state.order_book_column_open;
        cx.notify();
    }

    pub(super) fn close_order_book_column_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_state.order_book_column_open {
            self.menu_state.order_book_column_open = false;
            cx.notify();
        }
    }

    pub(super) fn close_side_panel(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        self.set_side_panel_visible(panel, false, cx);
    }

    pub(super) fn set_side_panel_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = clamped_side_panel_width(width);
        if (width - self.side_panel_width).abs() > f32::EPSILON {
            self.side_panel_width = width;
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_side_panel_split_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let basis_points = (ratio.clamp(0.05, 0.95) * 10_000.0)
            .round()
            .to_u32()
            .unwrap_or(5_000);
        if basis_points != self.side_panel_split_basis_points {
            self.side_panel_split_basis_points = basis_points;
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    /// Copies the chart capture; the task resolves `true` once the image is on the clipboard.
    pub(super) fn copy_chart_capture(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<bool>> {
        self.chart
            .as_ref()
            .map(|chart| chart.update(cx, AerisChartView::copy_capture))
    }

    pub(super) fn save_chart_capture(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, AerisChartView::save_capture);
        }
    }

    pub(super) fn reset_chart_view(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.reset_view();
                chart_cx.notify();
            });
        }
    }

    pub(super) fn select_drawing_tool(
        &mut self,
        tool: Option<ChartDrawingKind>,
        cx: &mut Context<Self>,
    ) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_drawing_tool(tool);
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    pub(super) fn remove_selected_chart_object(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_selected_chart_object() {
                    chart_cx.notify();
                }
            });
            self.retain_chart_presentation(cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_selected_drawing_lock(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let locked = chart.selected_drawing_locked();
                if chart.set_selected_drawing_locked(!locked) {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    pub(super) fn undo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(true, cx);
    }

    pub(super) fn redo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(false, cx);
    }

    fn step_drawing_history(&mut self, undo: bool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            let stepped = chart.update(cx, |chart, chart_cx| {
                let stepped = if undo {
                    chart.undo_drawing()
                } else {
                    chart.redo_drawing()
                };
                if stepped {
                    chart_cx.notify();
                }
                stepped
            });
            if stepped {
                cx.notify();
            }
        }
    }

    pub(super) fn clear_drawings(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_drawings();
                chart.cancel_drawing();
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    pub(super) fn open_study_settings_dialog(
        &mut self,
        study_id: StudyInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.studies.removing.contains(&study_id)
            || self.studies.reinitializing.contains_key(&study_id)
        {
            self.indicator_message = Some("Study settings are busy; try again".to_string());
            cx.notify();
            return;
        }
        let Some(active) = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .cloned()
        else {
            self.indicator_message = Some("Study is no longer active".to_string());
            cx.notify();
            return;
        };
        let series = match self.current_runtime_series() {
            Ok(series) => series,
            Err(message) => {
                self.indicator_message = Some(message);
                cx.notify();
                return;
            }
        };
        let registration =
            match runtime_study_registration(&active.persisted, &series, &self.studies.active) {
                Ok(registration) => registration,
                Err(message) => {
                    self.indicator_message = Some(message);
                    cx.notify();
                    return;
                }
            };
        let mut draft_values = BTreeMap::new();
        let mut inputs = HashMap::new();
        let mut subscriptions = Vec::new();
        let surface = cx.entity();
        for spec in &registration.definition.settings {
            let Some(value) = registration.settings.get(&spec.identifier).cloned() else {
                self.indicator_message = Some("Study settings are incomplete".to_string());
                cx.notify();
                return;
            };
            if let Some(text) = study_setting_input_text(&value) {
                let input = cx.new(|input_cx| InputState::new(window, input_cx));
                input.update(cx, |input, input_cx| {
                    input.set_value(text, window, input_cx);
                });
                let notify_surface = surface.clone();
                subscriptions.push(window.subscribe(
                    &input,
                    cx,
                    move |_, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            notify_surface.update(cx, |_, surface_cx| surface_cx.notify());
                        }
                    },
                ));
                inputs.insert(spec.identifier.clone(), input);
            }
            draft_values.insert(spec.identifier.clone(), value);
        }
        self.study_settings_dialog = Some(StudySettingsDialogState {
            study_id,
            title: study_display_name(&active.persisted.identifier),
            specs: registration.definition.settings,
            draft_values,
            line_width: persisted_study_line_width(&active.persisted),
            inputs,
            _subscriptions: subscriptions,
            message: None,
        });
        self.big_trades_dialog = None;
        self.indicator_message = None;
        self.chrome_overlay = None;
        self.timeframe_menu_flyout = None;
        self.chrome_selection = 0;
        cx.notify();
    }

    pub(super) fn set_study_settings_line_width(&mut self, width: u8, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.study_settings_dialog
            && dialog.line_width != width
        {
            dialog.line_width = width;
            cx.notify();
        }
    }

    /// Applies a study's stroke width to the chart and its durable state.
    fn apply_study_line_width(
        &mut self,
        study_id: StudyInstanceId,
        width: u8,
        cx: &mut Context<Self>,
    ) {
        if let Some(active) = self
            .studies
            .active
            .iter_mut()
            .find(|state| state.study_id == study_id)
            && active.persisted.line_width != u32::from(width)
        {
            active.persisted.line_width = u32::from(width);
            self.chart_persistence_dirty = true;
        }
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_study_line_width(study_id.get(), width) {
                    chart_cx.notify();
                }
            });
        }
    }

    pub(super) fn close_study_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if self.study_settings_dialog.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn remove_runtime_study(
        &mut self,
        study_id: StudyInstanceId,
        cx: &mut Context<Self>,
    ) {
        if self.studies.removing.contains(&study_id) {
            return;
        }
        if !self
            .studies
            .active
            .iter()
            .any(|state| state.study_id == study_id)
        {
            self.indicator_message = Some("Study is no longer active".to_string());
            cx.notify();
            return;
        }
        match self.market_worker.try_remove_study(study_id) {
            Ok(()) => {
                self.studies.removing.insert(study_id);
                if self
                    .study_settings_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.study_id == study_id)
                {
                    self.study_settings_dialog = None;
                }
                self.indicator_message = None;
            }
            Err(_) => {
                self.indicator_message =
                    Some("Study removal queue is busy or unavailable".to_string());
            }
        }
        cx.notify();
    }

    pub(super) fn set_study_setting_boolean(
        &mut self,
        identifier: &str,
        value: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.study_settings_dialog {
            dialog
                .draft_values
                .insert(identifier.to_string(), StudySettingValue::Boolean(value));
            dialog.message = None;
            cx.notify();
        }
    }

    pub(super) fn select_study_setting_choice(
        &mut self,
        identifier: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.study_settings_dialog {
            dialog.draft_values.insert(
                identifier.to_string(),
                StudySettingValue::Choice(value.to_string()),
            );
            dialog.message = None;
            cx.notify();
        }
    }

    pub(super) fn reset_study_settings_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.study_settings_dialog else {
            return;
        };
        let default_line_width = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == dialog.study_id)
            .map_or(DEFAULT_STUDY_LINE_WIDTH, |state| {
                default_study_line_width(&state.persisted.identifier)
            });
        for spec in &dialog.specs {
            let value = spec.default.clone();
            if let Some(text) = study_setting_input_text(&value)
                && let Some(input) = dialog.inputs.get(&spec.identifier)
            {
                input.update(cx, |input, input_cx| {
                    input.set_value(text, window, input_cx);
                });
            }
            dialog.draft_values.insert(spec.identifier.clone(), value);
        }
        dialog.line_width = default_line_width;
        dialog.message = None;
        cx.notify();
    }

    pub(super) fn save_study_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.study_settings_dialog.as_ref() else {
            return;
        };
        let study_id = dialog.study_id;
        if self.studies.reinitializing.contains_key(&study_id) {
            return;
        }
        let line_width = dialog.line_width;
        let values = dialog
            .specs
            .iter()
            .map(|spec| {
                study_setting_value_from_dialog(dialog, spec, cx)
                    .map(|value| (spec.identifier.clone(), value))
            })
            .collect::<Result<Vec<_>, _>>();
        let values = match values {
            Ok(values) => values,
            Err(message) => return self.show_study_settings_message(Some(message), cx),
        };
        let Some(active) = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .cloned()
        else {
            let message = "Study is no longer active".to_string();
            return self.show_study_settings_message(Some(message), cx);
        };
        self.apply_study_line_width(study_id, line_width, cx);
        let active_settings = active.persisted.settings.clone();
        let mut replacement = active.persisted;
        replacement.line_width = u32::from(line_width);
        replacement.settings = values
            .iter()
            .map(|(identifier, value)| WorkspaceStudySettingState {
                identifier: identifier.clone(),
                value: Some(persisted_study_setting_value(value)),
            })
            .collect();
        if replacement.settings == active_settings {
            // Only presentation changed; the runtime computation stays as it is.
            self.study_settings_dialog = None;
            cx.notify();
            return;
        }
        let registration = self.current_runtime_series().and_then(|series| {
            runtime_study_registration(&replacement, &series, &self.studies.active)
                .map(|registration| (series, registration))
        });
        let (series, registration) = match registration {
            Ok(registration) => registration,
            Err(message) => return self.show_study_settings_message(Some(message), cx),
        };
        if !self.retry_automatic_study_removals() {
            let message = "Study cancellation is waiting for runtime capacity".to_string();
            return self.show_study_settings_message(Some(message), cx);
        }
        let message = match self
            .market_worker
            .try_reinitialize_study(study_id, registration)
        {
            Ok(()) => {
                self.studies.begin_reinitialization(
                    study_id,
                    PendingStudyReinitialization {
                        series,
                        replacement_persisted: Some(replacement),
                    },
                );
                None
            }
            Err(TrySendError::Full(_)) => {
                Some("Study settings queue is busy; try again".to_string())
            }
            Err(TrySendError::Disconnected(_)) => Some("Study runtime is unavailable".to_string()),
        };
        self.show_study_settings_message(message, cx);
    }

    fn show_study_settings_message(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.study_settings_dialog {
            dialog.message = message;
        }
        cx.notify();
    }

    /// Opens a draft of the chart's big-trades settings. An automatic filter seeds the fixed
    /// minimum with the volume it currently applies, so switching to a fixed filter starts there.
    pub(super) fn open_big_trades_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((settings, threshold)) = self.chart.as_ref().map(|chart| {
            let chart = chart.read(cx);
            (
                chart.order_flow_settings().big_trades,
                chart.big_trades_threshold(),
            )
        }) else {
            return;
        };
        let Some(settings) = settings else {
            self.indicator_message = Some("Big trades is no longer on this chart".to_string());
            cx.notify();
            return;
        };
        let (intensity, fixed_minimum) = big_trades_filter_draft(settings.filter);
        let minimum_volume =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Order volume"));
        if let Some(volume) = fixed_minimum.or(threshold) {
            minimum_volume.update(cx, |input, input_cx| {
                input.set_value(big_trades_volume_text(volume), window, input_cx);
            });
        }
        self.big_trades_dialog = Some(BigTradesDialogState {
            intensity,
            size: settings.size,
            show_volume: settings.show_volume,
            minimum_volume,
            message: None,
        });
        self.study_settings_dialog = None;
        self.indicator_message = None;
        self.chrome_overlay = None;
        self.timeframe_menu_flyout = None;
        self.chrome_selection = 0;
        cx.notify();
    }

    pub(super) fn close_big_trades_dialog(&mut self, cx: &mut Context<Self>) {
        if self.big_trades_dialog.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn set_big_trades_dialog_filter(
        &mut self,
        intensity: Option<BigTradesIntensity>,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.big_trades_dialog
            && dialog.intensity != intensity
        {
            dialog.intensity = intensity;
            dialog.message = None;
            cx.notify();
        }
    }

    pub(super) fn set_big_trades_dialog_size(
        &mut self,
        size: BigTradesSize,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.big_trades_dialog
            && dialog.size != size
        {
            dialog.size = size;
            cx.notify();
        }
    }

    pub(super) fn set_big_trades_dialog_show_volume(
        &mut self,
        show_volume: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.big_trades_dialog
            && dialog.show_volume != show_volume
        {
            dialog.show_volume = show_volume;
            cx.notify();
        }
    }

    pub(super) fn reset_big_trades_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.big_trades_dialog else {
            return;
        };
        let defaults = BigTradesSettings::default();
        let (intensity, fixed_minimum) = big_trades_filter_draft(defaults.filter);
        if let Some(volume) = fixed_minimum {
            dialog.minimum_volume.update(cx, |input, input_cx| {
                input.set_value(big_trades_volume_text(volume), window, input_cx);
            });
        }
        dialog.intensity = intensity;
        dialog.size = defaults.size;
        dialog.show_volume = defaults.show_volume;
        dialog.message = None;
        cx.notify();
    }

    /// Applies the draft through the order-flow settings owner. Visibility stays with the legend.
    pub(super) fn apply_big_trades_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &self.big_trades_dialog else {
            return;
        };
        let filter = match dialog.intensity {
            Some(intensity) => Ok(BigTradesFilter::Auto { intensity }),
            None => {
                parse_big_trades_minimum_volume(dialog.minimum_volume.read(cx).value().as_ref())
                    .map(|minimum_volume| BigTradesFilter::Fixed { minimum_volume })
            }
        };
        let (size, show_volume) = (dialog.size, dialog.show_volume);
        let filter = match filter {
            Ok(filter) => filter,
            Err(message) => return self.show_big_trades_message(message, cx),
        };
        let Some(mut settings) = self.chart_order_flow_settings(cx) else {
            return self.show_big_trades_message("The chart is unavailable".to_string(), cx);
        };
        let Some(current) = settings.big_trades else {
            let message = "Big trades is no longer on this chart".to_string();
            return self.show_big_trades_message(message, cx);
        };
        settings.big_trades = Some(BigTradesSettings {
            filter,
            size,
            show_volume,
            visible: current.visible,
        });
        self.big_trades_dialog = None;
        self.set_chart_order_flow_settings(settings, cx);
        cx.notify();
    }

    fn show_big_trades_message(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.big_trades_dialog {
            dialog.message = Some(message);
        }
        cx.notify();
    }

    pub(super) fn clear_indicators(&mut self, cx: &mut Context<Self>) {
        let study_ids = self
            .studies
            .active
            .iter()
            .filter(|state| !self.studies.removing.contains(&state.study_id))
            .map(|state| state.study_id)
            .collect::<Vec<_>>();
        for study_id in study_ids {
            match self.market_worker.try_remove_study(study_id) {
                Ok(()) => {
                    self.studies.removing.insert(study_id);
                }
                Err(_) => {
                    self.indicator_message =
                        Some("Study removal queue is busy or unavailable".to_string());
                }
            }
        }
        let unregistered_removed = discard_unregistered_runtime_studies(&mut self.studies);
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.clear_indicators() {
                    chart_cx.notify();
                }
            });
        }
        if unregistered_removed {
            self.chart_persistence_dirty = true;
        }
        self.retain_chart_presentation(cx);
        cx.notify();
    }

    pub(super) fn has_removable_indicators(&self, cx: &App) -> bool {
        !self.studies.active.is_empty()
            || !self.studies.pending.is_empty()
            || !self.studies.deferred.is_empty()
            || self
                .chart
                .as_ref()
                .is_some_and(|chart| chart.read(cx).has_indicators())
    }

    pub(super) fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chart) = self.chart.clone() else {
            self.indicator_message = Some("Chart data is not available yet".to_string());
            cx.notify();
            return false;
        };
        let maximum = current_plan_limits().indicators_per_chart;
        let runtime_count = runtime_study_count(&self.studies);
        if chart.read(cx).indicator_states().len() + runtime_count >= maximum {
            self.indicator_message = Some(format!(
                "Your plan supports at most {maximum} indicators per chart"
            ));
            cx.notify();
            return false;
        }
        if runtime_managed_indicator(indicator) {
            if let Err(message) = self.current_runtime_series() {
                self.indicator_message = Some(message);
                cx.notify();
                return false;
            }
            let Some(local_id) = self.studies.allocate_local_id() else {
                self.indicator_message = Some("Study identity space is exhausted".to_string());
                cx.notify();
                return false;
            };
            let Some(persisted) = legacy_runtime_study(local_id, indicator, true) else {
                self.indicator_message = Some("Study implementation is unavailable".to_string());
                cx.notify();
                return false;
            };
            let state = PendingRuntimeStudyState {
                persisted,
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            };
            return match self.enqueue_runtime_study(state) {
                Ok(_) => {
                    self.indicator_message = None;
                    self.chart_persistence_dirty = true;
                    cx.notify();
                    true
                }
                Err(error) => {
                    let (_, message) = *error;
                    self.indicator_message = Some(message);
                    cx.notify();
                    false
                }
            };
        }
        let result = chart.update(cx, |chart, chart_cx| {
            let result = chart.add_indicator(indicator);
            if result.is_ok() {
                chart_cx.notify();
            }
            result
        });
        match result {
            Ok(_) => {
                self.indicator_message = None;
                self.retain_chart_presentation(cx);
                true
            }
            Err(error) => {
                self.indicator_message = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    /// Reports what the header's undo and redo controls may offer for the active chart.
    pub(super) fn drawing_history_state(&self, cx: &App) -> DrawingHistoryState {
        self.chart
            .as_ref()
            .map_or_else(DrawingHistoryState::default, |chart| {
                let chart = chart.read(cx);
                DrawingHistoryState {
                    can_undo: chart.can_undo_drawing(),
                    can_redo: chart.can_redo_drawing(),
                }
            })
    }

    pub(super) fn drawing_toolbar_state(&self, cx: &App) -> DrawingToolbarState {
        self.chart
            .as_ref()
            .map_or_else(DrawingToolbarState::default, |chart| {
                DrawingToolbarState::from_chart(chart.read(cx))
            })
    }
}

#[cfg(test)]
#[path = "workspace_surface_tests.rs"]
mod tests;
