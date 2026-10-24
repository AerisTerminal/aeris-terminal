//! Workspace surface.

use super::*;
use aeris_observability::diagnostic;

#[cfg(feature = "diagnostics")]
#[derive(serde::Serialize)]
struct SnapshotGap {
    start_utc_ns: i64,
    missing_intervals: i64,
}

#[cfg(feature = "diagnostics")]
fn snapshot_gaps(
    timestamps: impl IntoIterator<Item = i64>,
    interval_nanos: i64,
) -> (usize, Vec<SnapshotGap>, bool) {
    const MAXIMUM_RECORDED_GAPS: usize = 32;
    let mut previous: Option<i64> = None;
    let mut count = 0;
    let mut gaps = Vec::with_capacity(MAXIMUM_RECORDED_GAPS);
    for timestamp in timestamps {
        if let Some(start) = previous {
            let distance = timestamp.saturating_sub(start);
            if distance != interval_nanos {
                count += 1;
                if gaps.len() < MAXIMUM_RECORDED_GAPS {
                    gaps.push(SnapshotGap {
                        start_utc_ns: start.saturating_add(interval_nanos),
                        missing_intervals: distance
                            .div_euclid(interval_nanos)
                            .saturating_sub(1)
                            .max(0),
                    });
                }
            }
        }
        previous = Some(timestamp);
    }
    (count, gaps, count > MAXIMUM_RECORDED_GAPS)
}

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
        extend_order_lines_left: restored
            .and_then(|state| state.extend_order_lines_left)
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
    order_quantity: order_quantity::OrderQuantityControls,
    order_ticket: order_ticket::OrderTicketControls,
    chart_chrome: chart_chrome::ChartChromePreferences,
    restored_chart_state: Option<WorkspaceChartState>,
    retained_chart_presentation: RetainedChartPresentation,
    restored_rithmic: Option<(ChartInterval, Option<(i64, i64)>)>,
    interval: ChartInterval,
    symbol_browser: rithmic_shell::RithmicSymbolBrowser,
    studies: RuntimeStudiesState,
    price_alerts: Vec<WorkspacePriceAlertState>,
    price_alert_restore_status: PriceAlertRestoreStatus,
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

// One line per surface field; splitting the literal would only scatter the defaults.
#[allow(clippy::too_many_lines)]
fn workspace_surface_from_initialization(init: WorkspaceSurfaceInitialization) -> WorkspaceSurface {
    let mut chart_pane_controller = chart_pane_controller::ChartPaneController::default();
    if init.startup.chart.is_some() {
        chart_pane_controller.chart_installed();
    }
    WorkspaceSurface {
        chart: init.startup.chart,
        order_book: init.order_book,
        trade_tape: None,
        trade_sweeps: Arc::from([]),
        time_sales: TimeSalesState::default(),
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
        chart_failure: init.startup.chart_failure,
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
        provider_transport_rtt_nanos: None,
        trading_pnl: TradingPnlState::default(),
        order_quantity: init.order_quantity,
        order_ticket: init.order_ticket,
        chart_trading_visibility: init.chart_trading_visibility,
        symbol_browser: init.symbol_browser,
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
        studies: init.studies,
        indicator_settings_dialog: None,
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
        interval: init.interval,
        chart_pane_controller,
        rithmic_previous_selection: None,
        restored_viewport: init.restored_rithmic.and_then(|restored| restored.1),
        last_persisted_viewport: None,
        observed_chart: ObservedChart::default(),
        pending_chart_context_menu: None,
        pending_pane_activate: PaneActivationRequest::None,
        pending_settings_request: None,
        pending_study_remove_request: None,
        resource_class: ConsumerResourceClass::Foreground,
        chart_chrome: init.chart_chrome,
        retained_chart_presentation: init.retained_chart_presentation,
        restored_chart_state: init.restored_chart_state,
        chart_persistence_dirty: false,
        last_chart_user_state_revision: 0,
        order_book_demand: OrderBookDemand::Released,
        price_alerts: init.price_alerts,
        price_alert_dialog: None,
        price_alert_restore_status: init.price_alert_restore_status,
        pending_price_alert_announcements: VecDeque::new(),
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
) -> (Vec<WorkspacePriceAlertState>, PriceAlertRestoreStatus) {
    let alerts = restored_chart_state
        .map(|state| state.price_alerts.clone())
        .unwrap_or_default();
    let failed = !alerts.is_empty()
        && market_worker
            .try_replace_price_alerts(runtime_price_alerts(&alerts))
            .is_err();
    let status = if failed {
        PriceAlertRestoreStatus::PendingFailure
    } else {
        PriceAlertRestoreStatus::Handled
    };
    (alerts, status)
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

/// The first retained trade a chart update still needs. Keeping one trade the chart
/// already applied preserves the window's continuity evidence, and the trailing
/// `tail_window` trades are always projected for the sweep classifier.
fn order_flow_projection_start(
    trades: &[aeris_market_runtime::RetainedMarketTrade],
    resume_ordinal: Option<u64>,
    tail_window: usize,
) -> usize {
    let Some(applied) = resume_ordinal else {
        return 0;
    };
    let next = trades.partition_point(|trade| trade.ingestion_ordinal <= applied);
    next.saturating_sub(1)
        .min(trades.len().saturating_sub(tail_window))
}

fn chart_order_flow_trades(
    snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot,
    start: usize,
) -> Option<Vec<OrderFlowTrade>> {
    snapshot
        .trades
        .get(start..)?
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
                session_id: retained.trading_day.and_then(|day| u64::try_from(day).ok()),
            })
        })
        .collect()
}

fn initial_symbol_browser(_: &MarketWorkerStartup) -> rithmic_shell::RithmicSymbolBrowser {
    rithmic_shell::RithmicSymbolBrowser::default()
}

fn import_drawings(chart: &mut AerisChartView, drawings: &WorkspaceInstrumentDrawingsState) {
    if let Err(error) = chart.import_drawings_json(&drawings.chart_state_json) {
        diagnostic!("Aeris persisted drawings could not be restored: {error}");
    }
}

/// Keeps the charts of one workspace that show the same instrument on one drawing set. Each
/// committed edit reaches every sibling pane, and a chart that opened without drawings adopts a
/// sibling's: a chart showing the instrument first, then the set a pane remembers.
pub(super) fn share_workspace_drawings<'a>(
    panes: &(impl Iterator<Item = &'a Entity<WorkspaceSurface>> + Clone),
    cx: &mut App,
) {
    let siblings = |index: usize| {
        panes
            .clone()
            .enumerate()
            .filter(move |(sibling, _)| *sibling != index)
            .map(|(_, pane)| pane)
    };
    for (index, pane) in panes.clone().enumerate() {
        let Some(shared) = pane.update(cx, |surface, cx| surface.take_drawings_publication(cx))
        else {
            continue;
        };
        for sibling in siblings(index) {
            sibling.update(cx, |surface, cx| {
                surface.receive_shared_drawings(&shared, cx);
            });
        }
    }
    for (index, pane) in panes.clone().enumerate() {
        let Some(instrument) =
            pane.update(cx, |surface, cx| surface.take_drawings_peer_request(cx))
        else {
            continue;
        };
        let shared = siblings(index)
            .find_map(|sibling| sibling.read(cx).live_drawings(&instrument, cx))
            .or_else(|| {
                siblings(index).find_map(|sibling| {
                    sibling
                        .read(cx)
                        .retained_chart_presentation
                        .drawings
                        .remembered_for(&instrument)
                })
            });
        if let Some(shared) = shared {
            pane.update(cx, |surface, cx| {
                surface.receive_shared_drawings(&shared, cx);
            });
        }
    }
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

pub(super) const fn persisted_chart_legend(
    legend: ChartLegendOptions,
) -> WorkspaceChartLegendState {
    WorkspaceChartLegendState {
        show_symbol: Some(legend.symbol),
        show_symbol_values: Some(legend.symbol_values),
        show_indicators: Some(legend.indicators),
        show_indicator_values: Some(legend.indicator_values),
    }
}

/// The persisted legend, with every part absent from a legacy workspace or template shown.
pub(super) fn restored_chart_legend(
    legend: Option<WorkspaceChartLegendState>,
) -> ChartLegendOptions {
    let legend = legend.unwrap_or_default();
    ChartLegendOptions {
        symbol: legend.show_symbol.unwrap_or(true),
        symbol_values: legend.show_symbol_values.unwrap_or(true),
        indicators: legend.show_indicators.unwrap_or(true),
        indicator_values: legend.show_indicator_values.unwrap_or(true),
    }
}

pub(super) fn persisted_chart_appearance(
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

pub(super) fn restored_chart_appearance(
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
    let series_color =
        |color: &str| restored_series_color(color, appearance.area_bottom_color.is_empty());
    Some(ChartAppearanceSettings {
        background_color: restored_appearance_color(&appearance.background_color, |colors| {
            colors.background
        }),
        text_color: restored_appearance_color(&appearance.text_color, |colors| colors.text),
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
        line_color: series_color(&appearance.line_color),
        line_width,
        line_style,
        area_top_color: series_color(&appearance.area_top_color),
        area_bottom_color: series_color(&appearance.area_bottom_color),
        baseline_top_color: series_color(&appearance.baseline_top_color),
        baseline_bottom_color: series_color(&appearance.baseline_bottom_color),
    })
}

/// Axis colors were never flattened into theme defaults, so only an empty or theme marker
/// follows the theme.
pub(super) fn restored_chart_axis(
    appearance: &WorkspaceChartAppearanceState,
) -> ChartAxisAppearance {
    let color = |color: &str| {
        if color.is_empty() || color == THEME_APPEARANCE_COLOR {
            ChartAppearanceColor::Theme
        } else {
            ChartAppearanceColor::Custom(color.to_string())
        }
    };
    ChartAxisAppearance {
        border_color: color(&appearance.axis_border_color),
        text_color: color(&appearance.axis_text_color),
    }
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
/// An empty value comes from a layout saved before the field existed and also follows the theme.
fn restored_appearance_color(
    color: &str,
    role: fn(ChartThemeColors) -> &'static str,
) -> ChartAppearanceColor {
    let theme_default = [AerisChartTheme::Light, AerisChartTheme::Dark]
        .into_iter()
        .any(|theme| role(ChartThemeColors::for_theme(theme)).eq_ignore_ascii_case(color));
    if color.is_empty() || color == THEME_APPEARANCE_COLOR || theme_default {
        ChartAppearanceColor::Theme
    } else {
        ChartAppearanceColor::Custom(color.to_string())
    }
}

/// The colors Aeris Charts reported for an unset line, area or baseline color before those
/// fields carried provenance (its line, market up and market down fallbacks).
const LEGACY_SERIES_DEFAULT_COLORS: [&str; 3] = ["#2196f3", "#089981", "#f7525f"];

/// A line, area or baseline color. A `legacy` layout pinned whichever fallback the engine
/// reported, which painted an opaque area fill, so those fallbacks follow the default again.
fn restored_series_color(color: &str, legacy: bool) -> ChartAppearanceColor {
    let legacy_default = legacy
        && LEGACY_SERIES_DEFAULT_COLORS
            .iter()
            .any(|fallback| fallback.eq_ignore_ascii_case(color));
    if color.is_empty() || color == THEME_APPEARANCE_COLOR || legacy_default {
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
    OrderBookDepth,
}

impl OrderFlowMenuStudy {
    pub(super) const ALL: [Self; 4] = [
        Self::CumulativeDelta,
        Self::Delta,
        Self::BigTrades,
        Self::OrderBookDepth,
    ];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::CumulativeDelta => "Cumulative delta (CVD)",
            Self::Delta => "Delta histogram",
            Self::BigTrades => "Big trades",
            Self::OrderBookDepth => "Order book on chart (DOM)",
        }
    }

    pub(super) const fn is_shown(self, settings: OrderFlowSettings) -> bool {
        match self {
            Self::CumulativeDelta => settings.show_cumulative_delta,
            Self::Delta => settings.show_delta_histogram,
            Self::BigTrades => settings.big_trades.is_some(),
            Self::OrderBookDepth => settings.order_book_depth.is_some(),
        }
    }

    /// Whether the study is built from trade prints rather than the resting book.
    pub(super) const fn needs_trades(self) -> bool {
        !matches!(self, Self::OrderBookDepth)
    }
}

/// One addable row of the indicator menu. Rendering, arrow keys, Enter and scrolling all index
/// the same list: hidden order-flow studies first, then the matching native indicators.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IndicatorMenuEntry {
    OrderFlow(OrderFlowMenuStudy),
    Indicator(&'static chart_chrome::IndicatorSpec),
}

impl IndicatorMenuEntry {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::OrderFlow(study) => study.label(),
            Self::Indicator(spec) => spec.label,
        }
    }
}

/// Scroll-body child of a menu entry. Each non-empty group is preceded by its heading.
pub(super) fn indicator_menu_scroll_item(
    entries: &[IndicatorMenuEntry],
    selection: usize,
) -> Option<usize> {
    let entry = entries.get(selection)?;
    let order_flow_heading = usize::from(
        entries
            .first()
            .is_some_and(|first| matches!(first, IndicatorMenuEntry::OrderFlow(_))),
    );
    let indicators_heading = usize::from(matches!(entry, IndicatorMenuEntry::Indicator(_)));
    Some(selection + order_flow_heading + indicators_heading)
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

fn parse_volume_average_length(text: &str) -> Result<u16, String> {
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|length| VolumeSettings::AVERAGE_LENGTHS.contains(length))
        .ok_or_else(|| {
            format!(
                "Length must be a whole number from {} to {}",
                VolumeSettings::AVERAGE_LENGTHS.start(),
                VolumeSettings::AVERAGE_LENGTHS.end()
            )
        })
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

/// Chart depth levels drawn from an order-book frame, with the tick size their prices lie on,
/// or `None` while the book is recovering and has no trustworthy levels.
fn chart_order_book_depth(
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
        order_book_depth: settings.order_book_depth.map(|book| book.visible),
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
    // Tag 9 records whether the order book is added and, when it is, whether it is shown.
    let order_book_depth = match settings.order_book_depth {
        Some(visible) => Some(restored_order_book_depth(
            visible,
            settings.order_book_depth_options.as_ref(),
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
        ticks_per_row: settings.ticks_per_row,
    })
}

impl RejectedSavedPresentation {
    fn of(state: &WorkspaceChartState) -> Self {
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

    const fn any(self) -> bool {
        self.appearance || self.order_flow
    }
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
            volume: state.volume.as_ref().map(persisted_volume_settings),
        })
        .collect()
}

fn persisted_volume_settings(settings: &VolumeSettings) -> WorkspaceVolumeSettingsState {
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
fn restored_volume_settings(state: &WorkspaceVolumeSettingsState) -> Option<VolumeSettings> {
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
    studies.registration_retry = None;
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
    let period = aeris_market_runtime::legacy_series_period(series)
        .ok_or_else(|| "study series cadence is unsupported".to_string())?;
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

/// Line colors a persisted study draws with by output index; `None` keeps the chart default.
pub(super) fn persisted_study_output_colors(
    persisted: &WorkspaceChartStudyState,
) -> Vec<Option<String>> {
    persisted
        .output_colors
        .iter()
        .map(|color| (!color.is_empty()).then(|| color.clone()))
        .collect()
}

/// The durable form of a dialog's colors: one entry per output, or none when every output
/// keeps the chart default.
pub(super) fn persisted_output_colors(
    output_count: usize,
    drafts: &[StudyOutputColorDraft],
) -> Vec<String> {
    if drafts.iter().all(|draft| draft.color.is_none()) {
        return Vec::new();
    }
    let mut colors = vec![String::new(); output_count];
    for draft in drafts {
        if let (Some(slot), Some(color)) = (colors.get_mut(draft.output_index), &draft.color) {
            slot.clone_from(color);
        }
    }
    colors
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

pub(super) fn legacy_runtime_study(
    local_id: u64,
    indicator: ChartIndicator,
    visible: bool,
) -> Option<WorkspaceChartStudyState> {
    let (identifier, implementation_revision, settings, output_identifiers) =
        legacy_runtime_study_contract(indicator)?;
    Some(WorkspaceChartStudyState {
        line_width: 0,
        output_colors: Vec::new(),
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
    PriceAlertRestoreStatus,
) {
    let studies = restored_runtime_studies(restored_chart_state);
    let (price_alerts, price_alert_restore_status) =
        restore_price_alerts(restored_chart_state, market_worker);
    (studies, price_alerts, price_alert_restore_status)
}

fn initialize_restored_chart(
    chart: Option<&Entity<AerisChartView>>,
    chart_chrome: chart_chrome::ChartChromePreferences,
    cx: &mut Context<WorkspaceSurface>,
) {
    initialize_chart_chrome(chart, chart_chrome, cx);
    observe_chart(chart, cx);
}

/// Writes pane-owned presentation choices to the same document whether the chart is
/// loaded or still waiting for a covering snapshot.
struct PanePersistence<'a>(&'a WorkspaceSurface);

impl PanePersistence<'_> {
    fn apply_surface_fields(&self, state: &mut WorkspaceChartState) {
        let surface = self.0;
        state.time_zone.clone_from(&surface.chart_time_zone);
        state.show_order_management_lines =
            Some(surface.chart_trading_visibility.show_order_management_lines);
        state.show_execution_marks = Some(surface.chart_trading_visibility.show_execution_marks);
        state.extend_order_lines_left =
            Some(surface.chart_trading_visibility.extend_order_lines_left);
        state.context_panel_visible = surface.context_panel_visible;
        state.context_panel_tab = surface.context_panel_tab.persisted();
        state.context_panel_height = surface.context_panel_height.round().to_u32().unwrap_or(0);
        state.chart_link_group = u32::from(surface.chart_link_group);
        state.chart_link_flags = u32::from(surface.chart_link_flags);
    }

    fn presentation(&self, saved: Option<&WorkspaceChartState>) -> PanePresentation {
        let mut state = saved.cloned().unwrap_or_default();
        self.apply_surface_fields(&mut state);
        PanePresentation {
            state,
            has_saved_chart: saved.is_some(),
        }
    }
}

/// One snapshot of the pane's chart preferences. Pane-owned choices override a
/// retained chart document before either the startup or replacement chart sees it.
struct PanePresentation {
    state: WorkspaceChartState,
    has_saved_chart: bool,
}

impl PanePresentation {
    fn apply(
        &self,
        chart: &Entity<AerisChartView>,
        drawings: Option<&WorkspaceInstrumentDrawingsState>,
        cx: &mut Context<WorkspaceSurface>,
    ) -> bool {
        let state = &self.state;
        let indicators = state
            .indicators
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
        let crosshair_mode = u8::try_from(state.crosshair_mode)
            .ok()
            .filter(|mode| *mode <= 3);
        let appearance = state.appearance.as_ref().and_then(|appearance| {
            Some((
                restored_chart_appearance(appearance)?,
                restored_chart_axis(appearance),
            ))
        });
        let order_flow = state
            .order_flow
            .as_ref()
            .and_then(restored_order_flow_settings);
        let legend = restored_chart_legend(state.legend);
        chart.update(cx, |chart, _| {
            let mut failed = chart.set_time_zone(&state.time_zone).is_err();
            failed |= chart
                .set_order_lines_extend_left(state.extend_order_lines_left.unwrap_or(true))
                .is_err();
            if !self.has_saved_chart {
                return failed;
            }
            chart.set_legend_options(legend);
            if let Some(order_flow) = order_flow {
                failed |= chart.set_order_flow_settings(order_flow).is_err();
            }
            if let Some(chart_type) = chart_type {
                chart.set_chart_type(chart_type);
            }
            if let Some((appearance, axis)) = &appearance {
                chart.set_appearance_settings(appearance);
                chart.set_axis_appearance(axis);
            }
            failed |= chart.restore_indicator_states(&indicators).is_err();
            if let Some(drawings) = drawings {
                import_drawings(chart, drawings);
            }
            if let Some(price_axis) = price_axis {
                chart.restore_price_axis_menu_state(price_axis);
            }
            if let Some(mode) = crosshair_mode {
                chart.set_crosshair_mode(mode);
            }
            failed
        })
    }
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
                PanePersistence(self).apply_surface_fields(&mut state);
                state.instrument_drawings = self.retained_chart_presentation.drawings.remembered();
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
        let instrument_drawings = match self.retained_chart_presentation.drawings.persisted(chart) {
            Ok(drawings) => drawings,
            Err(error) => {
                diagnostic!("Aeris drawings could not be serialized: {error}");
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
        let mut state = WorkspaceChartState {
            chart_type: chart.chart_type().identifier().to_string(),
            chart_state_json: String::new(),
            indicators,
            price_axis,
            locked_drawing_ids: Vec::new(),
            crosshair_mode: u32::from(chart.crosshair_mode()),
            price_alerts: self.price_alerts.clone(),
            studies,
            appearance: Some(persisted_chart_appearance(
                &chart.appearance_settings(),
                &chart.axis_appearance(),
            )),
            order_flow: Some(persisted_order_flow_settings(chart.order_flow_settings())),
            legend: Some(persisted_chart_legend(chart.legend_options())),
            instrument_drawings,
            ..WorkspaceChartState::default()
        };
        PanePersistence(self).apply_surface_fields(&mut state);
        Some(state)
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
        order_quantity: order_quantity::OrderQuantityControls,
        order_ticket: order_ticket::OrderTicketControls,
        chart_chrome: chart_chrome::ChartChromePreferences,
        restored_chart_state: Option<WorkspaceChartState>,
    ) -> Self {
        let theme = AerisTheme::dark();
        let restored_rithmic = restored_market_selection(&startup);
        let symbol_browser = initial_symbol_browser(&startup);
        let startup_state = terminal_startup_state(startup, cx);
        let interval = restored_rithmic.map_or(ChartInterval::Minute1, |restored| restored.0);
        let (studies, price_alerts, price_alert_restore_status) =
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
        workspace_surface_from_initialization(WorkspaceSurfaceInitialization {
            startup: startup_state,
            market_worker,
            lifecycle,
            symbol_input,
            indicator_input,
            timeframe_input,
            time_zone_input,
            order_quantity,
            order_ticket,
            chart_chrome,
            retained_chart_presentation: RetainedChartPresentation {
                drawings: PaneDrawings::restored(
                    restored_chart_state
                        .as_ref()
                        .map_or(&[], |state| state.instrument_drawings.as_slice()),
                ),
                ..RetainedChartPresentation::default()
            },
            restored_chart_state,
            restored_rithmic,
            interval,
            symbol_browser,
            studies,
            price_alerts,
            price_alert_restore_status,
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

    /// The empty startup chart is created before the pane notice subscription exists. Apply
    /// its fallible settings only once that subscription is wired; later snapshot charts use
    /// `install_snapshot_chart` and its existing chart-restore notice.
    pub(super) fn apply_initial_chart_settings(&mut self, cx: &mut Context<Self>) {
        let Some(chart) = self.chart.as_ref() else {
            return;
        };
        let failed = PanePersistence(self)
            .presentation(None)
            .apply(chart, None, cx);
        if failed {
            Self::post_chart_restore_failure(cx);
        } else {
            Self::clear_indicator_failure("chart_restore", cx);
        }
    }

    fn post_chart_restore_failure(cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
            key: notification_center::NoticeKey {
                scope: cx.entity_id().as_u64(),
                kind: "chart_restore",
            },
            severity: notification_center::NoticeSeverity::Error,
            tone: notification_center::NoticeTone::Loss,
            source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
            title: "Chart settings could not be restored",
            detail: "Some saved chart settings could not be applied. Review this chart's settings."
                .into(),
            action: None,
            lifetime: notification_center::NoticeLifetime::UntilCleared,
        }));
        cx.notify();
    }

    pub(super) fn retire_market_worker(&mut self, cx: &App) {
        if let Some(retirement) = self.market_worker.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }

    pub(super) fn set_market_resource_class(
        &mut self,
        resource_class: ConsumerResourceClass,
        cx: &mut Context<Self>,
    ) {
        self.resource_class = resource_class;
        let key = notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "market_resource_class",
        };
        match self
            .market_worker
            .try_set_market_resource_class(resource_class)
        {
            Ok(()) => cx.emit(PaneNoticeEvent::Clear(key)),
            Err(TrySendError::Disconnected(_))
                if resource_class == ConsumerResourceClass::Detached =>
            {
                // The retiring pane has already lost its receiver; shutdown still
                // retires the consumer through the lifecycle owner.
                cx.emit(PaneNoticeEvent::Clear(key));
            }
            Err(TrySendError::Full(_)) if resource_class == ConsumerResourceClass::Detached => {
                diagnostic!("Detached pane market demand queue was full during retirement");
                cx.emit(PaneNoticeEvent::Clear(key));
            }
            Err(error) => {
                diagnostic!("Pane market demand could not be updated: {error}");
                cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
                    key,
                    severity: notification_center::NoticeSeverity::Warning,
                    tone: notification_center::NoticeTone::Warning,
                    source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
                    title: "Market demand unavailable",
                    detail: "This chart's market priority could not be updated. Try switching tabs again."
                        .into(),
                    action: None,
                    lifetime: notification_center::NoticeLifetime::UntilCleared,
                }));
            }
        }
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
            if let Err(error) = app.update_in(cx, |app, _, app_cx| {
                if app.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && app.timeframe_flyout_close_token == token
                {
                    app.close_timeframe_flyout(app_cx);
                }
            }) && app.upgrade().is_some()
            {
                diagnostic!("Live pane timeframe flyout close could not reach its window: {error}");
            }
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
        self.chrome_selection = super::provider_chart_types(self.provider)
            .iter()
            .position(|chart_type| *chart_type == selected)
            .unwrap_or(0);
    }

    fn apply_highlighted_chart_type(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chart_type) = super::provider_chart_types(self.provider)
            .get(self.chrome_selection)
            .copied()
        {
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
        let utc_seconds = self
            .lifecycle
            .clock()
            .wall_unix_nanos()
            .div_euclid(1_000_000_000);
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

    /// The selected chart time zone, owned by the surface so it reads correctly before the
    /// chart is created and across chart replacement.
    pub(super) fn chart_time_zone_id(&self) -> &str {
        &self.chart_time_zone
    }

    /// Header clock in the pane's selected zone. It needs no chart, so it shows at startup.
    pub(super) fn chart_time_zone_clock(&self) -> String {
        let utc_seconds = self
            .lifecycle
            .clock()
            .wall_unix_nanos()
            .div_euclid(1_000_000_000);
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
            Self::post_indicator_failure(
                "chart_time_zone",
                "Chart settings unavailable",
                "The selected chart time zone is unsupported.",
                cx,
            );
            return false;
        }
        if let Some(chart) = &self.chart
            && chart
                .update(cx, |chart, chart_cx| {
                    let changed = chart.set_time_zone(time_zone)?;
                    if changed {
                        chart_cx.notify();
                    }
                    Ok::<_, String>(changed)
                })
                .is_err()
        {
            Self::post_indicator_failure(
                "chart_time_zone",
                "Chart settings unavailable",
                "The chart time zone could not be applied.",
                cx,
            );
            return false;
        }
        Self::clear_indicator_failure("chart_time_zone", cx);
        if self.chart_time_zone != time_zone {
            self.chart_time_zone = time_zone.to_string();
            self.chart_persistence_dirty = true;
            cx.notify();
        }
        true
    }

    pub(super) fn post_indicator_failure(
        kind: &'static str,
        title: &'static str,
        detail: &'static str,
        cx: &mut Context<Self>,
    ) {
        cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
            key: notification_center::NoticeKey {
                scope: cx.entity_id().as_u64(),
                kind,
            },
            severity: notification_center::NoticeSeverity::Warning,
            tone: notification_center::NoticeTone::Warning,
            source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
            title,
            detail: detail.into(),
            action: None,
            lifetime: notification_center::NoticeLifetime::UntilCleared,
        }));
        cx.notify();
    }

    pub(super) fn clear_indicator_failure(kind: &'static str, cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Clear(notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind,
        }));
    }

    fn post_study_failure(detail: &'static str, cx: &mut Context<Self>) {
        Self::post_indicator_failure("study", "Study unavailable", detail, cx);
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
            Self::post_indicator_failure(
                "series_selection",
                "Timeframe unavailable",
                "This timeframe requires a paid plan.",
                cx,
            );
            cx.notify();
            return false;
        }
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| {
            if self.interval == interval && self.chart_pane_controller.pending_interval().is_none()
            {
                return true;
            }
            if self.chart_pane_controller.pending_interval() == Some(interval) {
                return true;
            }
            let Some(product) = self.product.clone() else {
                self.series_message = "Select a market before changing the timeframe.".into();
                Self::post_indicator_failure(
                    "series_selection",
                    "Timeframe unavailable",
                    "Select a market before changing the timeframe.",
                    cx,
                );
                cx.notify();
                return false;
            };
            let Ok(sequence) = self.market_worker.try_select_instrument(product, interval) else {
                self.series_message = format!("{} history could not start", interval.label());
                Self::post_indicator_failure(
                    "series_selection",
                    "Timeframe unavailable",
                    "Market history could not start. Try again.",
                    cx,
                );
                cx.notify();
                return false;
            };
            self.chart_pane_controller.request(sequence);
            self.chart_pane_controller
                .set_pending_target(sequence, interval, None);
            self.chart_state = ChartState::Loading;
            self.clear_chart_failure(cx);
            Self::clear_indicator_failure("series_selection", cx);
            self.series_message.clear();
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
        }) && self.chart_pane_controller.pending_product().is_none()
        {
            return true;
        }
        if self
            .chart_pane_controller
            .pending_product()
            .is_some_and(|pending| {
                pending.provider == instrument.provider
                    && pending.instrument_id == instrument.instrument_id
            })
        {
            return true;
        }
        let interval = super::instrument_interval(instrument, self.interval);
        let Ok(sequence) = self
            .market_worker
            .try_select_instrument(instrument.clone(), interval)
        else {
            Self::post_indicator_failure(
                "series_selection",
                "Market history unavailable",
                "Market history could not start. Try again.",
                cx,
            );
            cx.notify();
            return false;
        };
        self.pending_linked_instrument = Some(instrument.clone());
        self.chart_pane_controller.request(sequence);
        self.chart_pane_controller
            .set_pending_target(sequence, interval, Some(instrument.clone()));
        self.chart_state = ChartState::Loading;
        self.clear_chart_failure(cx);
        Self::clear_indicator_failure("series_selection", cx);
        self.series_message.clear();
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
        // An empty query opens the provider's own listing: its markets, or the
        // products whose contracts expand beneath them.
        if let Some(input) = &self.symbol_input {
            input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        self.search_symbol_query("", cx);
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
        self.symbol_browser.menu_entries()
    }

    /// Selects an instrument row or expands or collapses a product row. Reports whether a
    /// selection was scheduled, which is when a chart menu closes.
    pub(super) fn activate_instrument_entry(
        &mut self,
        action: &InstrumentMenuAction,
        target: SymbolSelectionTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        match action {
            InstrumentMenuAction::Select(selection) => {
                self.select_instrument(*selection, target, cx)
            }
            InstrumentMenuAction::ToggleProduct(product) => {
                if let Some(request) = self.symbol_browser.toggle_product(product) {
                    self.dispatch_symbol_search(request, cx);
                }
                cx.notify();
                false
            }
            InstrumentMenuAction::Inert => false,
        }
    }

    fn activate_chrome_list_keyboard(&mut self) {
        if matches!(
            self.chrome_overlay,
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator | ChromeOverlay::TimeZone)
        ) {
            self.menu_state.chrome_list_keyboard = true;
        }
    }

    /// The indicator menu has no current item, so nothing is highlighted until the user types
    /// or presses an arrow; that first arrow lands on the top row.
    const fn indicator_menu_awaits_keyboard(&self) -> bool {
        matches!(self.chrome_overlay, Some(ChromeOverlay::Indicator))
            && !self.menu_state.chrome_list_keyboard
    }

    fn scroll_chrome_selection_into_view(&self, cx: &App) {
        match self.chrome_overlay {
            Some(ChromeOverlay::Instrument) if !self.symbol_browser.results().is_empty() => {
                self.scrolls
                    .instrument
                    .scroll_to_item(self.chrome_selection);
            }
            Some(ChromeOverlay::Indicator) => {
                if let Some(item) = indicator_menu_scroll_item(
                    &self.indicator_menu_entries(cx),
                    self.chrome_selection,
                ) {
                    self.scrolls.indicator.scroll_to_item(item);
                }
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
            let provider = terminal_provider_id(self.symbol_provider);
            let Some(descriptor) = super::provider_presentation(self.symbol_provider) else {
                return false;
            };
            let Some(selection) = self.symbol_browser.select(selection) else {
                return false;
            };
            if target == SymbolSelectionTarget::Chart {
                let request = aeris_desktop::market_worker::CatalogSelectionRequest {
                    sequence: 0,
                    browser_generation: selection.generation.get() as u64,
                    search_generation: selection.search_generation.get() as u64,
                    provider: provider.to_string(),
                    symbol: selection.instrument.symbol.clone(),
                    exchange: selection.instrument.exchange.clone(),
                    entitlement_id: descriptor.selection_entitlement_id.to_string(),
                    interval: self.interval,
                };
                let Ok(sequence) = self.market_worker.try_select_catalog(request) else {
                    self.symbol_browser.reject_selection(selection.generation);
                    self.post_symbol_selection_notice(
                        "Market selection could not be scheduled. Try again.",
                        cx,
                    );
                    cx.notify();
                    return false;
                };
                self.chart_pane_controller.request(sequence);
                self.pending_symbol_selection_target = Some(target);
                self.market_state.symbol_selection_pending = true;
                cx.notify();
                return true;
            }
            let request = SelectProviderInstrument {
                startup_resolve: false,
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
                self.post_symbol_selection_notice(
                    "Market selection could not be scheduled. Try again.",
                    cx,
                );
                cx.notify();
                return false;
            }
            self.pending_symbol_selection_target = Some(target);
            self.market_state.symbol_selection_pending = true;
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
        if self.chrome_overlay == Some(overlay)
            && self.chrome_overlay_phase != ChromeOverlayPhase::Closing
        {
            self.close_chrome_overlay(window, cx);
            return;
        }
        if overlay == ChromeOverlay::Instrument {
            self.symbol_selection_target = SymbolSelectionTarget::Chart;
        }
        if overlay == ChromeOverlay::Accounts {
            self.menu_state.accounts_placement =
                chart_context_menus::FloatingPanelPlacement::default();
        }
        if matches!(overlay, ChromeOverlay::Instrument | ChromeOverlay::Accounts) {
            // Both surfaces present the broker connections; the runtime check reads the
            // local credential vault in the background and never blocks this frame.
            window.dispatch_action(Box::new(RefreshBrokerConnections), cx);
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
                self.refresh_instrument_listing(cx);
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
            if let Some(input) = &self.symbol_input {
                input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
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
                window.dispatch_action(Box::new(CancelRithmicLogin), cx);
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
            if let Err(error) = app.update_in(cx, |app, _, app_cx| {
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
            }) && app.upgrade().is_some()
            {
                diagnostic!("Live pane chrome close could not reach its window: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    /// Whether the surface lets chart shortcuts act: it has a chart and no chrome overlay or
    /// modal is open. Keyboard focus is the workspace's to judge.
    pub(super) fn chart_shortcuts_unblocked(&self) -> bool {
        self.chrome_overlay.is_none()
            && self.trading_pnl.account_creator.is_none()
            && self.trading_pnl.account_delete_confirmation.is_none()
            && self.indicator_settings_dialog.is_none()
            && self.chart.is_some()
    }

    /// Whether keyboard focus rests on this surface itself, its chrome or its chart, rather
    /// than on one of its text fields or on drawing text being edited.
    pub(super) fn holds_keyboard_focus(&self, window: &Window, cx: &App) -> bool {
        self.chrome_focus.is_focused(window)
            || self.chart.as_ref().is_some_and(|chart| {
                let chart = chart.read(cx);
                chart.has_keyboard_focus(window) && !chart.is_editing_text()
            })
    }

    /// Returns keyboard focus to the surface: its chart, or its chrome before a chart exists.
    pub(super) fn focus_keyboard(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.chart.clone() {
            Some(chart) => chart.update(cx, |chart, cx| chart.focus_keyboard(window, cx)),
            None => self.chrome_focus.focus(window, cx),
        }
    }

    /// Keys while a modal is open: `None` when no modal is, otherwise whether the key was
    /// consumed. Text editing inside a modal owns ordinary key input, so typed characters never
    /// reach the workspace-wide symbol and timeframe typeahead as chart shortcuts.
    fn modal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let escape = event.keystroke.key.eq_ignore_ascii_case("escape");
        if self.trading_pnl.account_creator.is_some() {
            if escape {
                self.trading_pnl.account_creator = None;
                cx.notify();
            }
            return Some(escape);
        }
        if self.trading_pnl.account_delete_confirmation.is_some() {
            if escape {
                self.trading_pnl.account_delete_confirmation = None;
                cx.notify();
            }
            return Some(escape);
        }
        if self.indicator_settings_dialog.is_some() {
            let drawing = escape && self.drawing_settings_dialog_open();
            if drawing {
                self.escape_drawing_settings_dialog(window, cx);
            }
            return Some(drawing);
        }
        None
    }

    pub(super) fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(handled) = self.modal_key_down(event, window, cx) {
            return handled;
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
                let first_keyboard_step = self.indicator_menu_awaits_keyboard();
                self.activate_chrome_list_keyboard();
                self.chrome_selection = if first_keyboard_step {
                    0
                } else {
                    self.chrome_selection.saturating_sub(1)
                };
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
                    Some(ChromeOverlay::Indicator) => self.indicator_menu_entries(cx).len(),
                    Some(ChromeOverlay::Timeframe) => self.timeframe_menu_keyboard_count(),
                    Some(ChromeOverlay::ChartType) => {
                        super::provider_chart_types(self.provider).len()
                    }
                    Some(ChromeOverlay::QuickTimeframe) => self.quick_timeframe_matches(cx).len(),
                    Some(ChromeOverlay::TimeZone) => self.time_zone_matches(cx).len(),
                    // The Accounts panel is a form, not a keyboard-navigated list.
                    Some(ChromeOverlay::Accounts) | None => 0,
                };
                let first_keyboard_step = self.indicator_menu_awaits_keyboard();
                self.activate_chrome_list_keyboard();
                self.chrome_selection = if first_keyboard_step {
                    0
                } else {
                    (self.chrome_selection + 1).min(count.saturating_sub(1))
                };
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
                let gap_evidence = interval_nanos.map(|interval_nanos| {
                    snapshot_gaps(
                        snapshot
                            .bars()
                            .iter()
                            .map(|bar| bar.provenance().exchange_timestamp_unix_nanos),
                        interval_nanos,
                    )
                });
                let first_timestamp = snapshot
                    .bars()
                    .first()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let last_timestamp = snapshot
                    .bars()
                    .last()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let interior_gaps = gap_evidence
                    .as_ref()
                    .map_or_else(|| "null".to_string(), |(count, _, _)| count.to_string());
                let gaps = gap_evidence.as_ref().map_or_else(
                    || "[]".to_string(),
                    |(_, gaps, _)| serde_json::json!(gaps).to_string(),
                );
                let gaps_truncated = gap_evidence
                    .as_ref()
                    .is_some_and(|(_, _, truncated)| *truncated);
                let interval_nanos =
                    interval_nanos.map_or_else(|| "null".to_string(), |value| value.to_string());
                diagnostic!(
                    "AERIS_LIVE_SNAPSHOT {{\"bar_count\":{},\"first_timestamp\":{first_timestamp},\"last_timestamp\":{last_timestamp},\"interior_gaps\":{interior_gaps},\"interval_nanos\":{interval_nanos},\"gaps\":{gaps},\"gaps_truncated\":{gaps_truncated}}}",
                    snapshot.bars().len()
                );
            }
            ReplayStreamUpdate::Delta(delta) => diagnostic!(
                "AERIS_LIVE_UPDATE {{\"kind\":\"delta\",\"timestamp\":{}}}",
                delta.item().provenance().exchange_timestamp_unix_nanos
            ),
            ReplayStreamUpdate::Tail(tail) => diagnostic!(
                "AERIS_LIVE_UPDATE {{\"kind\":\"tail\",\"timestamp\":{},\"forming\":{}}}",
                tail.item().provenance().exchange_timestamp_unix_nanos,
                tail.forming()
            ),
        }
    }

    /// Replaces the chart with one built from a covering snapshot and every retained surface
    /// state, before it is first drawn.
    pub(super) fn install_snapshot_chart(
        &mut self,
        snapshot: &aeris_application::ReplaySnapshot,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme;
        let chart = cx.new(move |_| AerisChartView::with_replay_and_theme(snapshot, theme));
        self.apply_chart_chrome_to_chart(&chart, cx);
        // An open settings session previews unconfirmed edits; restore the drawing before
        // its state is carried into the new chart.
        self.close_drawing_settings_dialog(false, cx);
        self.remember_chart_drawings(cx);
        let drawings = self
            .retained_chart_presentation
            .drawings
            .begin_chart(self.product.as_ref());
        let mut restore_failed = if let Some(restored) = self.restored_chart_state.take() {
            self.retained_chart_presentation.rejected_saved =
                RejectedSavedPresentation::of(&restored);
            let drawings = drawings.or_else(|| {
                chart_drawings::legacy_pane_drawings(
                    &restored.chart_state_json,
                    &restored.locked_drawing_ids,
                    self.product.as_ref(),
                )
            });
            PanePersistence(self)
                .presentation(Some(&restored))
                .apply(&chart, drawings.as_ref(), cx)
        } else if let Some(state) = &self.retained_chart_presentation.chart_state {
            PanePersistence(self)
                .presentation(Some(state))
                .apply(&chart, drawings.as_ref(), cx)
        } else {
            self.apply_retained_indicators_to_chart(&chart, cx);
            if let Some(drawings) = drawings {
                chart.update(cx, |chart, _| import_drawings(chart, &drawings));
            }
            PanePersistence(self)
                .presentation(None)
                .apply(&chart, None, cx)
        };
        restore_failed |= self.retained_chart_presentation.rejected_saved.any();
        if restore_failed {
            Self::post_chart_restore_failure(cx);
        } else {
            Self::clear_indicator_failure("chart_restore", cx);
        }
        self.apply_market_session_to_chart(&chart, cx);
        self.apply_provider_trade_prints(&chart, cx);
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
        self.observed_chart = ObservedChart {
            clock_revision: chart.read(cx).clock_revision(),
            selected_drawing: chart.read(cx).selected_drawing_id(),
        };
        self.retained_chart_presentation
            .drawings
            .settle_chart(chart.read(cx));
        observe_chart(Some(&chart), cx);
        self.chart = Some(chart);
        self.chart_pane_controller.chart_installed();
        self.sync_order_book_demand(cx);
        // Interval switches keep the instrument's tape; project it into the new chart now
        // instead of leaving the footprint empty until the next tape revision.
        if let Some(tape) = self.trade_tape.clone() {
            self.apply_trade_tape_to_chart(&tape, cx);
        }
        self.apply_order_book_to_chart(cx);
        self.synchronize_runtime_studies(cx);
    }

    /// A provider without trade prints has nothing to draw a footprint or tape study from,
    /// so a restored or retained chart shows candles and no tape panes instead of blank ones.
    fn apply_provider_trade_prints(&self, chart: &Entity<AerisChartView>, cx: &mut Context<Self>) {
        if super::provider_trades_available(self.provider) {
            return;
        }
        chart.update(cx, |chart, _| {
            if chart.chart_type() == ChartType::Footprint {
                chart.set_chart_type(ChartType::Candles);
            }
            let settings = chart.order_flow_settings();
            let without_tape = OrderFlowSettings {
                show_cumulative_delta: false,
                show_delta_histogram: false,
                big_trades: None,
                ..settings
            };
            if without_tape != settings
                && let Err(error) = chart.set_order_flow_settings(without_tape)
            {
                diagnostic!("Aeris tape studies could not be removed: {error}");
            }
        });
    }

    pub(super) fn apply_publication(
        &mut self,
        publication: MarketWorkerPublication,
        cx: &mut Context<Self>,
    ) {
        if !self
            .chart_pane_controller
            .accept_publication(publication.generation.selection_generation())
        {
            return;
        }
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
        let swapping = self.chart_pane_controller.chart_needs_replacement();
        let next_state = match (&self.chart, update) {
            (existing, aeris_application::ReplayStreamUpdate::Snapshot(snapshot))
                if existing.is_none() || swapping =>
            {
                self.install_snapshot_chart(&snapshot, cx);
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
                        diagnostic!(
                            "chart rejected a market update; canonical resnapshot required"
                        );
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
            self.clear_chart_failure(cx);
            if super::provider_presentation(self.provider).is_some() {
                self.market_state.symbol_selection_pending = false;
                if !self.chart_pane_controller.chart_needs_replacement() {
                    self.rithmic_previous_selection = None;
                }
            }
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
    }

    fn apply_runtime_series_publication(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        received_unix_nanos: i64,
        cx: &mut Context<Self>,
    ) {
        use aeris_market_runtime::MarketRuntimeEvent;
        let (generation, provider, update) = match event.value {
            MarketRuntimeEvent::SeriesSnapshot(snapshot) => {
                let generation = snapshot.generation.0.get();
                if !self.chart_pane_controller.accept_publication(generation) {
                    return;
                }
                let provider = snapshot.snapshot.series.provider_id.clone();
                let replay = engine_market_worker::replay_runtime_snapshot(
                    &snapshot,
                    self.product.as_ref(),
                    received_unix_nanos,
                )
                .map(ReplayStreamUpdate::Snapshot);
                (generation, provider, replay)
            }
            MarketRuntimeEvent::SeriesUpdate(tail) => {
                let generation = tail.generation.0.get();
                if !self.chart_pane_controller.accept_publication(generation) {
                    return;
                }
                let provider = tail.series.provider_id.clone();
                let replay =
                    engine_market_worker::replay_runtime_tail_update(&tail, received_unix_nanos)
                        .map(ReplayStreamUpdate::Tail);
                (generation, provider, replay)
            }
            _ => {
                diagnostic!("Aeris pane received an unexpected runtime bar event");
                return;
            }
        };
        let update = match update {
            Ok(update) => update,
            Err(error) => {
                self.set_chart_state(ChartState::Error, error, cx);
                return;
            }
        };
        let evidence = match &update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                MarketPublicationGeneration::from_snapshot(snapshot)
            }
            ReplayStreamUpdate::Tail(tail) => {
                let sequence = tail.item().value().source_sequence;
                MarketPublicationGeneration::from_tail(
                    tail.publication_generation(),
                    0,
                    sequence,
                    sequence,
                )
            }
            ReplayStreamUpdate::Delta(_) => unreachable!("runtime publishes snapshots or tails"),
        };
        let (subscription_id, worker_label) = engine_market_worker::worker_identity(&provider);
        self.apply_publication(
            MarketWorkerPublication {
                update,
                generation: evidence.with_selection_generation(generation),
                subscription_id: subscription_id.to_string(),
                worker_label: worker_label.to_string(),
                ui_diagnostics: None,
            },
            cx,
        );
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

    fn apply_recovery_message(
        &mut self,
        generation: u64,
        request_id: u64,
        failure: MarketRecoveryFailure,
        cx: &mut Context<Self>,
    ) {
        // A recovery result is a lifecycle outcome, not an incremental bar.
        // The still-displayed series cannot settle a replacement request.
        if self.chart_pane_controller.accept_series_state(generation) {
            self.apply_recovery(request_id, Err(failure), cx);
        }
    }

    fn apply_runtime_recovery(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        request_id: u64,
        received_unix_nanos: i64,
        cx: &mut Context<Self>,
    ) {
        let aeris_market_runtime::MarketRuntimeEvent::SeriesSnapshot(snapshot) = event.value else {
            diagnostic!("Aeris pane received an unexpected recovery event");
            return;
        };
        let generation = snapshot.generation.0.get();
        if !self.chart_pane_controller.accept_series_state(generation)
            || self
                .product
                .as_ref()
                .is_none_or(|product| product.provider != snapshot.snapshot.series.provider_id)
        {
            return;
        }
        let replay = match engine_market_worker::replay_runtime_snapshot(
            &snapshot,
            self.product.as_ref(),
            received_unix_nanos,
        ) {
            Ok(replay) => replay,
            Err(error) => {
                self.apply_recovery(request_id, Err(MarketRecoveryFailure::Failed(error)), cx);
                return;
            }
        };
        let (subscription_id, worker_label) =
            engine_market_worker::worker_identity(&snapshot.snapshot.series.provider_id);
        self.apply_recovery(
            request_id,
            Ok(Box::new(MarketWorkerBootstrap {
                generation: MarketPublicationGeneration::from_snapshot(&replay)
                    .with_selection_generation(generation),
                snapshot: replay,
                subscription_id: subscription_id.to_string(),
                worker_label: worker_label.to_string(),
            })),
            cx,
        );
    }

    fn apply_recovery(
        &mut self,
        request_id: u64,
        result: Result<Box<MarketWorkerBootstrap>, MarketRecoveryFailure>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            // The replacement request or selection delivers its own covering snapshot, which
            // replaces this chart; retrying or reporting the retired request would be wrong.
            Err(MarketRecoveryFailure::Superseded) => return,
            Err(MarketRecoveryFailure::Failed(error)) => {
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
                diagnostic!("fixture recovery {request_id} failed: {error}");
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
                    bootstrap.generation,
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
                diagnostic!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.mark_replay_stream_invalid();
                chart_cx.notify();
            });
        }
        diagnostic!("market worker invalidated the stream");
    }

    /// Reports whether a selection handoff must keep the chart covered.
    ///
    /// Until the replacement's covering snapshot and its readiness have both
    /// arrived, the pixels belong to the series the trader just left, whether
    /// the instrument or only the interval changed. Showing them uncovered
    /// presents the old series as the new selection and then pops the
    /// replacement in without any loading state.
    pub(super) fn showing_superseded_series(&self) -> bool {
        super::provider_presentation(self.provider).is_some()
            && self.chart.is_some()
            && (self.chart_pane_controller.has_pending_selection()
                || self.chart_pane_controller.chart_needs_replacement()
                || (self.rithmic_previous_selection.is_some()
                    && self.chart_state != ChartState::Ready))
    }

    fn set_chart_state(&mut self, state: ChartState, _message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(cx);
        }
        if state == ChartState::Error {
            self.chart_failure = Some(ChartFailure::Market);
            Self::post_chart_failure(ChartFailure::Market, cx);
        } else if matches!(
            state,
            ChartState::Ready | ChartState::AwaitingData | ChartState::HistoryUnavailable
        ) {
            self.clear_chart_failure(cx);
            Self::clear_indicator_failure("series_selection", cx);
        }
        self.chart_state = state;
        cx.notify();
    }

    fn post_chart_failure(failure: ChartFailure, cx: &mut Context<Self>) {
        Self::post_indicator_failure("chart_state", "Chart unavailable", failure.detail(), cx);
    }

    fn clear_chart_failure(&mut self, cx: &mut Context<Self>) {
        self.chart_failure = None;
        Self::clear_indicator_failure("chart_state", cx);
    }

    pub(super) fn emit_initial_chart_failure_notice(&mut self, cx: &mut Context<Self>) {
        if let Some(failure) = self.chart_failure {
            Self::post_chart_failure(failure, cx);
        }
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
            MarketWorkerMessage::RuntimeProjection(projection) => {
                if let Some(received_unix_nanos) = projection.received_unix_nanos {
                    if let Some(request_id) = projection.recovery_request_id {
                        self.apply_runtime_recovery(
                            projection.event,
                            request_id,
                            received_unix_nanos,
                            cx,
                        );
                    } else {
                        self.apply_runtime_series_publication(
                            projection.event,
                            received_unix_nanos,
                            cx,
                        );
                    }
                } else {
                    self.apply_fenced_runtime_event(
                        projection.event,
                        projection.selection_generation,
                        cx,
                    );
                }
            }
            MarketWorkerMessage::LocalFailure { scope, message } => {
                self.apply_local_failure(scope, message, cx);
            }
            MarketWorkerMessage::SessionStatus { generation, status } => match status {
                aeris_desktop::market_worker::LocalSessionStatus::Connection {
                    state,
                    transport_rtt_nanos,
                } => {
                    self.apply_market_connection_message(
                        generation,
                        state,
                        transport_rtt_nanos,
                        cx,
                    );
                }
                aeris_desktop::market_worker::LocalSessionStatus::MarketSession(status) => {
                    self.apply_fenced_market_session_status(generation, status, cx);
                }
            },
            MarketWorkerMessage::LocalCommandOutcome(outcome) => {
                self.apply_local_command_outcome(outcome, cx);
            }
        }
    }

    fn apply_local_command_outcome(
        &mut self,
        outcome: aeris_desktop::market_worker::LocalCommandOutcome,
        cx: &mut Context<Self>,
    ) {
        use aeris_desktop::market_worker::LocalCommandOutcome;
        match outcome {
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded,
            } => {
                if succeeded {
                    self.commit_pending_selection(sequence, cx);
                } else {
                    self.reject_pending_engine_selection(sequence, cx);
                }
            }
            LocalCommandOutcome::CatalogSwitch {
                sequence,
                browser_generation,
                result,
            } => match result {
                Ok((instrument, interval)) => {
                    self.apply_catalog_switch(
                        sequence,
                        browser_generation,
                        instrument,
                        interval,
                        cx,
                    );
                }
                Err(_) => self.reject_catalog_switch(sequence, browser_generation, cx),
            },
            LocalCommandOutcome::SelectedInstrument {
                generation,
                instrument,
            } => self.apply_selected_instrument(generation, instrument),
            LocalCommandOutcome::Recovery {
                generation,
                request_id,
                failure,
            } => self.apply_recovery_message(generation, request_id, failure, cx),
            LocalCommandOutcome::PriceAlertSync {
                sequence,
                succeeded,
            } => self.apply_current_price_alert_sync(sequence, succeeded, cx),
            LocalCommandOutcome::StudyRegistration {
                request_sequence,
                result,
            } => match result {
                Ok(study_id) => self.apply_study_registered(request_sequence, study_id, cx),
                Err(message) => self.apply_study_registration_failed(request_sequence, message, cx),
            },
            LocalCommandOutcome::StudyReinitialization { study_id, result } => {
                if result.is_ok() {
                    self.apply_study_reinitialized(study_id, cx);
                } else {
                    self.apply_study_reinitialization_failure(study_id, cx);
                }
            }
            LocalCommandOutcome::StudyRemovalFailed { study_id } => {
                self.apply_study_removal_failure(study_id, cx);
            }
        }
    }

    fn apply_local_failure(
        &mut self,
        scope: aeris_desktop::market_worker::LocalFailureScope,
        message: String,
        cx: &mut Context<Self>,
    ) {
        match scope {
            aeris_desktop::market_worker::LocalFailureScope::Catalog => {
                Self::post_symbol_search_notice(
                    "The market catalog is unavailable. Try again later.",
                    cx,
                );
            }
            aeris_desktop::market_worker::LocalFailureScope::Selection(generation) => {
                self.apply_series_state_message(generation, ChartState::Error, message, cx);
            }
        }
    }

    fn apply_fenced_runtime_event(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        selection_generation: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if selection_generation.is_some()
            && !matches!(
                &event.value,
                aeris_market_runtime::MarketRuntimeEvent::ProviderState(_)
            )
        {
            diagnostic!("Aeris pane received unexpected selection-stamped runtime event");
            return;
        }
        match &event.value {
            aeris_market_runtime::MarketRuntimeEvent::ProviderState(_) => {
                if let Some(generation) = selection_generation {
                    self.apply_runtime_provider_state(event, generation, cx);
                } else {
                    diagnostic!("Aeris pane received provider state without a selection fence");
                }
            }
            aeris_market_runtime::MarketRuntimeEvent::SeriesState(_)
            | aeris_market_runtime::MarketRuntimeEvent::DemandError(_) => {
                self.apply_runtime_series_status(event, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::OrderBookSnapshot(_) => {
                self.apply_runtime_order_book(event, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::TradeTapeSnapshot(_) => {
                self.apply_runtime_trade_tape(event, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::StudyOutputSnapshot(_)
            | aeris_market_runtime::MarketRuntimeEvent::StudyOutputsInvalidated(_)
            | aeris_market_runtime::MarketRuntimeEvent::StudyRemoved(_) => {
                self.apply_runtime_study(event, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::PriceAlertTriggered(_) => {
                self.apply_runtime_price_alert(event, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::CatalogReady { .. }
            | aeris_market_runtime::MarketRuntimeEvent::ProviderInstrumentSearchResult(_)
            | aeris_market_runtime::MarketRuntimeEvent::ProviderInstrumentSelection(_)
            | aeris_market_runtime::MarketRuntimeEvent::ProviderCatalogRejected { .. }
            | aeris_market_runtime::MarketRuntimeEvent::ProviderMarketScreen(_)
            | aeris_market_runtime::MarketRuntimeEvent::ProviderMarketScreenRejected(_) => {
                self.apply_runtime_catalog_event(event, cx);
            }
            _ => diagnostic!("Aeris pane received an unexpected fenced runtime event"),
        }
    }

    fn apply_fenced_market_session_status(
        &mut self,
        generation: u64,
        status: aeris_contracts::MarketSessionStatus,
        cx: &mut Context<Self>,
    ) {
        if self.chart_pane_controller.accept_publication(generation) {
            self.apply_market_session_status(status, cx);
        }
    }

    fn apply_runtime_price_alert(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        if let aeris_market_runtime::MarketRuntimeEvent::PriceAlertTriggered(trigger) = event.value
        {
            self.apply_price_alert_trigger(&trigger, cx);
        } else {
            diagnostic!("Aeris pane received an unexpected runtime price-alert event");
        }
    }

    fn apply_selected_instrument(
        &mut self,
        generation: u64,
        instrument: InstallProviderInstrument,
    ) {
        if self.product.as_ref().is_some_and(|product| {
            product.provider == instrument.provider
                && product.instrument_id == instrument.instrument_id
        }) && self
            .chart_pane_controller
            .adopt_initial_instrument(generation)
        {
            self.product = Some(instrument);
            self.chart_persistence_dirty = true;
        }
    }

    fn apply_market_session_status(
        &mut self,
        status: aeris_contracts::MarketSessionStatus,
        cx: &mut Context<Self>,
    ) {
        // Catalog selection can announce the next instrument before its series
        // selection commits. Do not show that session under the previous chart.
        if self
            .product
            .as_ref()
            .is_none_or(|product| product.instrument_id != status.instrument_id)
        {
            return;
        }
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
        if !self
            .chart_pane_controller
            .accept_publication(snapshot.generation.0.get())
        {
            return;
        }
        let is_current = self.product.as_ref().is_none_or(|product| {
            snapshot.provider_id == product.provider
                && snapshot.instrument_id == product.instrument_id
                && snapshot.entitlement_id == product.entitlement_id
        });
        if is_current
            && self.trade_tape.as_ref().is_none_or(|current| {
                current.generation != snapshot.generation
                    || (snapshot.provider_generation >= current.provider_generation
                        && (current.provider_generation != snapshot.provider_generation
                            || snapshot.revision >= current.revision))
            })
        {
            self.apply_trade_tape_to_chart(&snapshot, cx);
            self.trade_tape = Some(snapshot);
            cx.notify();
        }
    }

    fn apply_runtime_trade_tape(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        if let aeris_market_runtime::MarketRuntimeEvent::TradeTapeSnapshot(snapshot) = event.value {
            self.apply_trade_tape(snapshot, cx);
        } else {
            diagnostic!("Aeris pane received an unexpected runtime trade-tape event");
        }
    }

    fn apply_runtime_study(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        match event.value {
            aeris_market_runtime::MarketRuntimeEvent::StudyOutputSnapshot(snapshot) => {
                self.apply_study_output_message(&snapshot, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::StudyOutputsInvalidated(invalidated) => {
                self.apply_study_invalidation_message(&invalidated, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::StudyRemoved(removed) => {
                self.apply_study_removal_message(&removed, cx);
            }
            _ => diagnostic!("Aeris pane received an unexpected runtime study event"),
        }
    }

    fn apply_runtime_order_book(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        let aeris_market_runtime::MarketRuntimeEvent::OrderBookSnapshot(snapshot) = event.value
        else {
            diagnostic!("Aeris pane received an unexpected runtime order book event");
            return;
        };
        if !self
            .chart_pane_controller
            .accept_publication(snapshot.generation.0.get())
        {
            return;
        }
        let Some(product) = &self.product else {
            return;
        };
        // An ancillary malformed or stale image leaves the last valid depth
        // visible; it must never put the price chart into an error state.
        if let Some(frame) = engine_market_worker::runtime_order_book_frame(
            &snapshot,
            product,
            snapshot.generation.0.get(),
        ) {
            self.apply_order_book_frame(frame, cx);
        }
    }

    fn apply_order_book_frame(
        &mut self,
        frame: aeris_market_data::OrderBookFrame,
        cx: &mut Context<Self>,
    ) {
        if self
            .chart_pane_controller
            .accept_publication(frame.selection_generation)
        {
            let installed = self.order_book.update(cx, |order_book, order_book_cx| {
                order_book.replace_frame(frame, order_book_cx)
            });
            if installed {
                self.apply_order_book_to_chart(cx);
            }
        }
    }

    /// Projects the pane's current book, owned by its order-book view, into the chart's
    /// right-edge depth bars while that indicator is shown.
    fn apply_order_book_to_chart(&self, cx: &mut Context<Self>) {
        let Some(chart) = &self.chart else {
            return;
        };
        if !chart
            .read(cx)
            .order_flow_settings()
            .order_book_depth
            .is_some_and(|book| book.visible)
        {
            return;
        }
        let Some((identity, depth)) = self
            .order_book
            .read(cx)
            .frame()
            .map(|frame| (frame.instrument_id.clone(), chart_order_book_depth(frame)))
        else {
            return;
        };
        let drawn = chart.update(cx, |chart, chart_cx| {
            let drawn = if let Some((tick_size, bids, asks)) = depth {
                chart.apply_order_book_depth(&identity, tick_size, bids, asks)
            } else {
                chart.clear_order_book_depth();
                Ok(())
            };
            chart_cx.notify();
            drawn
        });
        if let Err(error) = drawn {
            diagnostic!("Aeris order book could not be drawn on the chart: {error}");
        }
    }

    /// Removes the chart's drawn book when the pane's book stops or changes instrument.
    fn clear_chart_order_book_depth(&self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_order_book_depth();
                chart_cx.notify();
            });
        }
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
        let series = self.current_runtime_series().ok();
        let now = Instant::now();
        if !self.studies.registration_due(series.as_ref(), now) {
            return;
        }
        let state = self.studies.deferred.remove(index);
        match self.enqueue_runtime_study(state) {
            Ok(_) => {
                self.studies.registration_retry = None;
                cx.notify();
            }
            Err(error) => {
                let (state, reason) = *error;
                // A blocked study is never retried, so its one failure is reported now;
                // anything else waits before the same series is tried again.
                let report = state.blocked || self.studies.defer_registration(series, now);
                self.studies.deferred.insert(index, state);
                if report {
                    diagnostic!("Aeris study could not be queued: {reason}");
                    Self::post_indicator_failure(
                        "study",
                        "Study unavailable",
                        "The study could not be queued. Try again.",
                        cx,
                    );
                }
            }
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
        let now = Instant::now();
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
                    && self
                        .studies
                        .reinitialization_due(state.study_id, &series, now)
            })
            .map(|state| state.study_id)
            .collect::<Vec<_>>();
        for study_id in stale {
            if let Err(reason) = self.queue_runtime_study_reinitialization(study_id, series.clone())
                && self
                    .studies
                    .defer_reinitialization(study_id, series.clone(), now)
            {
                diagnostic!("Aeris study could not be updated: {reason}");
                Self::post_indicator_failure(
                    "study",
                    "Study unavailable",
                    "The study could not be updated. Try again.",
                    cx,
                );
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
            if !self.retry_automatic_study_removals() {
                // The pending removal stays owned by RuntimeStudiesState and
                // subsequent study synchronization retries it.
                diagnostic!("Automatic study removal is waiting for market worker capacity");
            }
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
        Self::clear_indicator_failure("study", cx);
        self.synchronize_runtime_studies(cx);
        cx.notify();
    }

    fn apply_study_registration_failed(
        &mut self,
        request_sequence: u64,
        _message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.studies.pending.remove(&request_sequence) else {
            return;
        };
        if state.persist_on_registration {
            self.chart_persistence_dirty = true;
        }
        if state.remove_on_registration {
            cx.notify();
            return;
        }
        if !state.persist_on_registration {
            let mut state = state;
            state.blocked = true;
            self.studies.deferred.insert(0, state);
        }
        Self::post_indicator_failure(
            "study",
            "Study unavailable",
            "The study could not be added. Try again.",
            cx,
        );
    }

    fn apply_study_reinitialized(&mut self, study_id: StudyInstanceId, cx: &mut Context<Self>) {
        let Some(persisted_changed) = self.studies.complete_reinitialization(study_id) else {
            return;
        };
        if persisted_changed {
            self.chart_persistence_dirty = true;
        }
        if self
            .study_settings_dialog()
            .is_some_and(|dialog| dialog.study_id == study_id)
        {
            self.indicator_settings_dialog = None;
        }
        Self::clear_indicator_failure("study", cx);
        cx.notify();
    }

    fn apply_study_reinitialization_failure(
        &mut self,
        study_id: StudyInstanceId,
        cx: &mut Context<Self>,
    ) {
        self.studies
            .reject_reinitialization(study_id, Instant::now());
        if let Some(dialog) = self.study_settings_dialog_mut()
            && dialog.study_id == study_id
        {
            dialog.message = Some("Study settings could not be applied. Try again.".into());
        }
        Self::post_study_failure("Study settings could not be applied. Try again.", cx);
    }

    fn apply_study_output_message(
        &self,
        snapshot: &aeris_market_runtime::MarketStudyOutputSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self
            .chart_pane_controller
            .accept_study_publication(snapshot.generation.map(|generation| generation.0.get()))
        {
            self.apply_study_output(snapshot, cx);
        }
    }

    fn apply_study_invalidation_message(
        &mut self,
        invalidated: &aeris_market_runtime::MarketStudyOutputsInvalidated,
        cx: &mut Context<Self>,
    ) {
        if self
            .chart_pane_controller
            .accept_study_publication(invalidated.generation.map(|generation| generation.0.get()))
        {
            self.apply_study_invalidated(invalidated, cx);
        }
    }

    fn apply_study_removal_message(
        &mut self,
        removed: &aeris_market_runtime::MarketStudyRemoved,
        cx: &mut Context<Self>,
    ) {
        if self
            .chart_pane_controller
            .accept_study_publication(removed.generation.map(|generation| generation.0.get()))
        {
            self.apply_study_removed(removed, cx);
        }
    }

    fn apply_study_removal_failure(&mut self, study_id: StudyInstanceId, cx: &mut Context<Self>) {
        if !study_removal_failed(&mut self.studies, study_id) {
            Self::post_indicator_failure(
                "study",
                "Study unavailable",
                "The study could not be removed. Try again.",
                cx,
            );
        }
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
        let output_colors = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == snapshot.study_id)
            .map(|state| persisted_study_output_colors(&state.persisted))
            .unwrap_or_default();
        chart.update(cx, |chart, chart_cx| {
            chart.set_study_line_width(snapshot.study_id.get(), line_width);
            chart.set_study_output_colors(snapshot.study_id.get(), &output_colors);
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
                    diagnostic!("Aeris study output could not be displayed: {error}");
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
            Self::clear_indicator_failure("study", cx);
        }
        if self
            .study_settings_dialog()
            .is_some_and(|dialog| removed.study_ids.contains(&dialog.study_id))
        {
            self.indicator_settings_dialog = None;
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

    fn apply_series_state_message(
        &mut self,
        generation: u64,
        state: ChartState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if self.chart_pane_controller.accept_series_state(generation) {
            self.apply_market_state_message(state, message, cx);
        }
    }

    fn apply_runtime_series_state(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        use aeris_contracts::SeriesLoadState;

        let aeris_market_runtime::MarketRuntimeEvent::SeriesState(state) = event.value else {
            diagnostic!("Aeris pane received an unexpected runtime readiness event");
            return;
        };
        if !self
            .chart_pane_controller
            .accept_series_state(state.generation.0.get())
        {
            return;
        }
        if state.presentation
            == Some(aeris_market_runtime::SeriesPresentationStatus::HistoryUnavailable)
        {
            // The runtime owns this phase: bars come from the live trade
            // stream while one slow covering-history probe stays armed.
            self.apply_market_state_message(
                ChartState::HistoryUnavailable,
                state
                    .detail
                    .unwrap_or_else(|| "History unavailable; showing live bars".to_string()),
                cx,
            );
            return;
        }
        let projected = match state.state {
            SeriesLoadState::Live => {
                Some((ChartState::Ready, "Market history/live handoff is current"))
            }
            SeriesLoadState::Ready => Some((
                ChartState::Loading,
                "Market history is loaded; connecting the live edge",
            )),
            SeriesLoadState::Resolving => Some((
                ChartState::Loading,
                "Market runtime is loading current coverage",
            )),
            SeriesLoadState::Partial if self.chart_state.awaits_engine_readiness() => Some((
                ChartState::Loading,
                "Market runtime is loading current coverage",
            )),
            SeriesLoadState::Empty => Some((
                ChartState::AwaitingData,
                "Market has no history for this period yet",
            )),
            SeriesLoadState::Failed => Some((ChartState::Error, "Market realtime failed")),
            SeriesLoadState::Partial | SeriesLoadState::Superseded => None,
        };
        if let Some((chart_state, fallback)) = projected {
            self.apply_market_state_message(
                chart_state,
                state.detail.unwrap_or_else(|| fallback.to_string()),
                cx,
            );
        }
    }

    fn apply_runtime_series_status(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        match &event.value {
            aeris_market_runtime::MarketRuntimeEvent::SeriesState(_) => {
                self.apply_runtime_series_state(event, cx);
            }
            aeris_market_runtime::MarketRuntimeEvent::DemandError(_) => {
                self.apply_runtime_demand_error(event, cx);
            }
            _ => diagnostic!("Aeris pane received an unexpected runtime series status"),
        }
    }

    fn apply_runtime_demand_error(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        let aeris_market_runtime::MarketRuntimeEvent::DemandError(error) = event.value else {
            diagnostic!("Aeris pane received an unexpected runtime demand error");
            return;
        };
        if !self
            .chart_pane_controller
            .accept_series_state(error.generation.0.get())
        {
            return;
        }
        if error.scope == aeris_market_runtime::MarketDemandErrorScope::OlderHistory {
            diagnostic!("Aeris chart older history is unavailable");
            return;
        }
        let state = if error.code == aeris_contracts::EngineFaultCode::Retryable {
            ChartState::Recovering
        } else {
            ChartState::Error
        };
        self.apply_market_state_message(state, error.detail, cx);
    }

    fn apply_market_state_message(
        &mut self,
        state: ChartState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let engine_provider = super::provider_presentation(self.provider).is_some();
        if state == ChartState::Error && engine_provider {
            let restoring_chart = self.chart_pane_controller.chart_needs_replacement()
                && self.chart.is_some()
                && self.rithmic_previous_selection.is_some();
            let instrument_changed = self
                .rithmic_previous_selection
                .as_ref()
                .is_some_and(|(product, _)| product != &self.product);
            self.chart_pane_controller.clear();
            self.market_state.symbol_selection_pending = false;
            if restoring_chart {
                self.restore_rithmic_selection_after_failure(cx);
                // The displayed chart still belongs to the previous selection.
                // A rejected replacement is a selection failure, not a failure
                // of that chart; keep it usable while its demand is restored.
                self.clear_chart_failure(cx);
                self.chart_state = ChartState::Ready;
                if instrument_changed {
                    self.post_symbol_selection_notice("Market selection failed. Try again.", cx);
                } else {
                    Self::post_indicator_failure(
                        "series_selection",
                        "Timeframe unavailable",
                        "Market history could not start. Try again.",
                        cx,
                    );
                }
                cx.notify();
                return;
            }
            self.rithmic_previous_selection = None;
        } else if engine_provider && state == ChartState::Ready {
            if !self.chart_pane_controller.chart_needs_replacement() {
                self.rithmic_previous_selection = None;
            }
            self.market_state.symbol_selection_pending = false;
        } else if engine_provider && state == ChartState::HistoryUnavailable {
            // The selection is installed and live; only covering history is
            // still catching up, so the instrument handoff is covered.
            self.market_state.symbol_selection_pending = false;
        } else if engine_provider && state == ChartState::AwaitingData {
            // Coverage may arrive before readiness; keep the instrument handoff
            // covered until both have arrived.
            self.market_state.symbol_selection_pending = false;
        }
        self.set_chart_state(state, message, cx);
    }

    fn apply_market_connection_message(
        &mut self,
        generation: u64,
        state: FeedConnectionState,
        transport_rtt_nanos: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if !self.chart_pane_controller.accept_publication(generation) {
            return;
        }
        self.provider_transport_rtt_nanos = if state == FeedConnectionState::Streaming {
            transport_rtt_nanos
        } else {
            None
        };
        self.apply_connection_state(state, cx);
    }

    fn apply_runtime_provider_state(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let aeris_market_runtime::MarketRuntimeEvent::ProviderState(state) = event.value else {
            diagnostic!("Aeris pane received an unexpected runtime provider event");
            return;
        };
        if self
            .product
            .as_ref()
            .is_none_or(|product| product.provider != state.provider)
        {
            return;
        }
        let connection = match state.state {
            aeris_contracts::ProviderConnectionState::Disconnected => {
                FeedConnectionState::Disconnected
            }
            aeris_contracts::ProviderConnectionState::Connecting => {
                FeedConnectionState::Discovering
            }
            aeris_contracts::ProviderConnectionState::Online => FeedConnectionState::Streaming,
            aeris_contracts::ProviderConnectionState::Recovering => FeedConnectionState::Recovering,
            aeris_contracts::ProviderConnectionState::Failed => FeedConnectionState::Stopped,
        };
        self.apply_market_connection_message(generation, connection, state.transport_rtt_nanos, cx);
    }

    fn apply_runtime_catalog_event(
        &mut self,
        event: aeris_market_runtime::MarketConsumerEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some(event) = project_provider_catalog_event(event.value) {
            self.apply_catalog_event(event, cx);
        }
    }

    fn apply_catalog_results(
        &mut self,
        generation: u64,
        instruments: Vec<ProviderInstrumentSummary>,
    ) -> bool {
        usize_generation(generation)
            .is_some_and(|generation| self.symbol_browser.apply_results(generation, instruments))
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

    fn apply_catalog_switch(
        &mut self,
        sequence: u64,
        browser_generation: u64,
        instrument: InstallProviderInstrument,
        interval: ChartInterval,
        cx: &mut Context<Self>,
    ) {
        if !self.chart_pane_controller.is_pending(sequence)
            || !self.confirm_catalog_selection(browser_generation)
        {
            return;
        }
        Self::clear_symbol_selection_notice(cx);
        self.pending_symbol_selection_target = None;
        self.consume_catalog_search_authorization();
        self.market_state.symbol_selection_pending = false;
        self.pending_linked_instrument = Some(instrument.clone());
        self.chart_pane_controller
            .set_pending_target(sequence, interval, Some(instrument));
        self.commit_pending_selection(sequence, cx);
    }

    fn reject_catalog_switch(
        &mut self,
        sequence: u64,
        browser_generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.chart_pane_controller.reject(sequence)
            && usize_generation(browser_generation)
                .is_some_and(|generation| self.symbol_browser.reject_selection(generation))
        {
            self.market_state.symbol_selection_pending = false;
            self.pending_symbol_selection_target = None;
            self.post_symbol_selection_notice("Market selection failed. Try again.", cx);
            cx.notify();
        }
    }

    fn reject_pending_engine_selection(&mut self, sequence: u64, cx: &mut Context<Self>) {
        let failed_interval = self.chart_pane_controller.pending_interval();
        let instrument_switch = self.chart_pane_controller.pending_product().is_some();
        if !self.chart_pane_controller.reject(sequence) {
            return;
        }
        self.pending_linked_instrument = None;
        self.market_state.symbol_selection_pending = false;
        self.chart_state = if self.chart.is_some() {
            ChartState::Ready
        } else {
            ChartState::Loading
        };
        if !instrument_switch && failed_interval.is_some_and(|interval| interval != self.interval) {
            Self::post_indicator_failure(
                "series_selection",
                "Timeframe unavailable",
                "Market history could not start. Try again.",
                cx,
            );
        } else {
            self.post_symbol_selection_notice("Market selection failed. Try again.", cx);
        }
        cx.notify();
    }

    /// Commits the exact pending selection's identity without touching the chart.
    ///
    /// The selection outcome says "everything after this belongs to the new
    /// selection". The chart the trader is looking at is left on screen — still
    /// its own series, still correct — under a loading notice, and is replaced
    /// in `apply_publication` when the replacement's covering snapshot arrives.
    /// Dropping it here is what produced the blank surface on every switch.
    pub(super) fn commit_pending_selection(&mut self, sequence: u64, cx: &mut Context<Self>) {
        if super::provider_presentation(self.provider).is_none()
            || !self.chart_pane_controller.accept(sequence)
        {
            return;
        }
        self.rithmic_previous_selection = Some((self.product.clone(), self.interval));
        // Capture presentation against the chart's current instrument before
        // mutating the pending selection. Timeframe replacement can reuse
        // drawings; a symbol replacement must not carry price-anchored drawings
        // onto another instrument.
        self.retain_chart_presentation(cx);
        if let Some(target) = self.chart_pane_controller.take_pending_target() {
            self.interval = target.interval;
            if let Some(product) = target.product {
                // Selections only come from providers the menu lists.
                if let Some(provider) = known_terminal_provider(&product.provider)
                    && provider != self.provider
                {
                    self.provider = provider;
                }
                self.product = Some(product);
                self.market_session_status = None;
                // Price levels belong to one instrument: a product switch drops
                // the old book back to loading instead of showing BTC levels
                // under an ETH selection. Interval-only switches keep the book.
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.clear(order_book_cx);
                });
                self.clear_chart_order_book_depth(cx);
                self.trade_tape = None;
                self.trade_sweeps = Arc::from([]);
                self.time_sales.rows = time_sales_panel::TimeSalesRowsCache::default();
            }
        }
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
    fn restore_rithmic_selection_after_failure(&mut self, cx: &mut Context<Self>) {
        let Some((product, interval)) = self.rithmic_previous_selection.take() else {
            return;
        };
        self.product.clone_from(&product);
        self.interval = interval;
        self.chart_pane_controller.clear();
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.clear(order_book_cx);
        });
        self.clear_chart_order_book_depth(cx);
        self.trade_tape = None;
        self.trade_sweeps = Arc::from([]);
        self.time_sales.rows = time_sales_panel::TimeSalesRowsCache::default();
        let restored = product.and_then(|product| {
            self.market_worker
                .try_select_instrument(product, interval)
                .ok()
        });
        if let Some(sequence) = restored {
            self.chart_pane_controller.request(sequence);
            self.chart_pane_controller
                .set_pending_target(sequence, interval, None);
        }
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
        let chart_update_received = messages.iter().any(|message| {
            matches!(
                message,
                MarketWorkerMessage::RuntimeProjection(
                    aeris_desktop::market_worker::RuntimeProjection {
                        received_unix_nanos: Some(_),
                        ..
                    }
                )
            )
        });
        for message in messages {
            self.apply_market_worker_message(message, cx);
        }
        if disconnected && !matches!(self.connection_state, Some(FeedConnectionState::Stopped)) {
            self.apply_connection_state(FeedConnectionState::Stopped, cx);
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

    fn apply_connection_state(&mut self, state: FeedConnectionState, cx: &mut Context<Self>) {
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
        }
        if state == FeedConnectionState::Stopped {
            if self.chart_failure.is_none()
                || self.chart_failure == Some(ChartFailure::WorkerStopped)
            {
                self.chart_failure = Some(ChartFailure::WorkerStopped);
                Self::post_chart_failure(ChartFailure::WorkerStopped, cx);
            }
        } else if state == FeedConnectionState::Streaming
            && self.chart_failure == Some(ChartFailure::WorkerStopped)
        {
            self.clear_chart_failure(cx);
            if self.chart_state == ChartState::Error {
                self.chart_state = ChartState::Loading;
            }
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
                self.clear_chart_order_book_depth(cx);
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
                self.clear_chart_order_book_depth(cx);
            }
        }
        cx.notify();
    }

    pub(super) fn apply_theme(&mut self, theme: &AerisTheme, cx: &mut Context<Self>) {
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.set_theme(*theme, order_book_cx);
        });
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(*theme);
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
            // A false result means unchanged or no target series on an empty chart,
            // not a failed settings write; retained precision remains authoritative.
            chart.apply_price_axis_menu_action(
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
            self.remember_chart_drawings(cx);
            self.retained_chart_presentation.chart_state = self.workspace_chart_state(cx);
        }
    }

    fn take_drawings_publication(&mut self, cx: &App) -> Option<SharedDrawings> {
        let chart = self.chart.as_ref()?.read(cx);
        self.retained_chart_presentation
            .drawings
            .take_publication(chart)
            .unwrap_or_else(|error| {
                diagnostic!("Aeris drawings could not be shared: {error}");
                None
            })
    }

    fn take_drawings_peer_request(&mut self, cx: &App) -> Option<DrawingInstrument> {
        let chart = self.chart.as_ref()?.read(cx);
        self.retained_chart_presentation
            .drawings
            .take_peer_request(chart)
    }

    fn live_drawings(&self, instrument: &DrawingInstrument, cx: &App) -> Option<SharedDrawings> {
        let chart = self.chart.as_ref()?.read(cx);
        self.retained_chart_presentation
            .drawings
            .live(instrument, chart)
            .unwrap_or_else(|error| {
                diagnostic!("Aeris drawings could not be shared: {error}");
                None
            })
    }

    fn receive_shared_drawings(&mut self, shared: &SharedDrawings, cx: &mut Context<Self>) {
        let drawings = &mut self.retained_chart_presentation.drawings;
        let received = match self.chart.clone() {
            Some(chart) => chart.update(cx, |chart, chart_cx| {
                let received = drawings.receive(shared, Some(chart));
                if !matches!(received, Ok(false)) {
                    chart_cx.notify();
                }
                received
            }),
            None => drawings.receive(shared, None),
        };
        match received {
            Ok(false) => return,
            Ok(true) => {}
            Err(error) => diagnostic!("Aeris shared drawings could not be applied: {error}"),
        }
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    fn remember_chart_drawings(&mut self, cx: &App) {
        let Some(chart) = &self.chart else {
            return;
        };
        if let Err(error) = self
            .retained_chart_presentation
            .drawings
            .remember(chart.read(cx))
        {
            diagnostic!("Aeris drawings could not be serialized: {error}");
        }
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
            diagnostic!("Aeris chart indicators could not be restored: {error}");
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

    pub(super) fn chart_theme_defaults(&self, cx: &App) -> Option<ChartThemeDefaults> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).theme_defaults())
    }

    pub(super) fn chart_axis_appearance(&self, cx: &App) -> Option<ChartAxisAppearance> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).axis_appearance())
    }

    pub(super) fn chart_series_colors(&self, cx: &App) -> Option<ChartSeriesColors> {
        self.chart
            .as_ref()
            .and_then(|chart| chart.read(cx).series_colors())
    }

    pub(super) fn chart_crosshair_mode(&self, cx: &App) -> Option<u8> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).crosshair_mode())
    }

    pub(super) fn chart_legend_options(&self, cx: &App) -> Option<ChartLegendOptions> {
        self.chart
            .as_ref()
            .map(|chart| chart.read(cx).legend_options())
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
        let visibility_changed = self.chart_trading_visibility.show_order_management_lines
            != visibility.show_order_management_lines
            || self.chart_trading_visibility.show_execution_marks
                != visibility.show_execution_marks;
        let extension_changed = self.chart_trading_visibility.extend_order_lines_left
            != visibility.extend_order_lines_left;
        self.chart_trading_visibility = visibility;
        if let Some(chart) = &self.chart {
            if visibility_changed {
                let current = chart.read(cx).trading_snapshot();
                let filtered =
                    workspace_layout::apply_chart_trading_visibility(current, visibility);
                chart.update(cx, |chart, chart_cx| {
                    if chart.set_trading_snapshot(filtered).is_ok() {
                        chart_cx.notify();
                    }
                });
            }
            if extension_changed {
                chart.update(cx, |chart, chart_cx| {
                    match chart.set_order_lines_extend_left(visibility.extend_order_lines_left) {
                        Ok(true) => chart_cx.notify(),
                        Ok(false) => {}
                        Err(error) => {
                            diagnostic!(
                                "Aeris chart order-line extension could not be applied: {error}"
                            );
                        }
                    }
                });
            }
        }
        // Enabling either layer needs the next authoritative runtime snapshot to restore any
        // presentation objects that were intentionally filtered out while hidden.
        if visibility_changed {
            self.trading_pnl.next_refresh = std::time::Instant::now();
        }
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    /// The indicator menu's addable rows for the current search, in display order.
    pub(super) fn indicator_menu_entries(&self, cx: &App) -> Vec<IndicatorMenuEntry> {
        let query = self.indicator_input.read(cx).value().to_string();
        let normalized_query = query.trim().to_ascii_lowercase();
        let trades = super::provider_trades_available(self.provider);
        let order_flow = self
            .chart_order_flow_settings(cx)
            .into_iter()
            .flat_map(|settings| {
                OrderFlowMenuStudy::ALL.into_iter().filter(move |study| {
                    (trades || !study.needs_trades()) && !study.is_shown(settings)
                })
            })
            .filter(|study| {
                study
                    .label()
                    .to_ascii_lowercase()
                    .contains(&normalized_query)
            })
            .map(IndicatorMenuEntry::OrderFlow);
        let indicators = chart_chrome::filter_indicator_specs(&query)
            .into_iter()
            .map(IndicatorMenuEntry::Indicator);
        order_flow.chain(indicators).collect()
    }

    pub(super) fn add_indicator_menu_entry(
        &mut self,
        entry: IndicatorMenuEntry,
        cx: &mut Context<Self>,
    ) -> bool {
        match entry {
            IndicatorMenuEntry::OrderFlow(study) => self.add_order_flow_study(study, cx),
            IndicatorMenuEntry::Indicator(spec) => {
                self.add_indicator(super::native_indicator(spec.kind), cx)
            }
        }
    }

    /// Adds a tape-derived order-flow indicator through the order-flow settings owner.
    pub(super) fn add_order_flow_study(
        &mut self,
        study: OrderFlowMenuStudy,
        cx: &mut Context<Self>,
    ) -> bool {
        if study.needs_trades() && !super::provider_trades_available(self.provider) {
            return false;
        }
        let Some(mut settings) = self.chart_order_flow_settings(cx) else {
            return false;
        };
        match study {
            OrderFlowMenuStudy::CumulativeDelta => settings.show_cumulative_delta = true,
            OrderFlowMenuStudy::Delta => settings.show_delta_histogram = true,
            OrderFlowMenuStudy::BigTrades => {
                settings.big_trades = Some(settings.big_trades.unwrap_or_default());
            }
            OrderFlowMenuStudy::OrderBookDepth => {
                settings.order_book_depth = Some(OrderBookDepthSettings {
                    visible: true,
                    ..settings.order_book_depth.unwrap_or_default()
                });
            }
        }
        self.set_chart_order_flow_settings(settings, cx);
        true
    }

    /// Applies `settings` through the chart, returning whether the chart holds them now.
    pub(super) fn set_chart_order_flow_settings(
        &mut self,
        settings: OrderFlowSettings,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chart) = &self.chart else {
            return false;
        };
        match chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_order_flow_settings(settings)?;
            if changed {
                chart_cx.notify();
            }
            Ok::<_, String>(changed)
        }) {
            Ok(true) => {
                Self::clear_indicator_failure("order_flow", cx);
                self.retained_chart_presentation.rejected_saved.order_flow = false;
                self.chart_persistence_dirty = true;
                if let Some(snapshot) = self.trade_tape.clone() {
                    self.apply_trade_tape_to_chart(&snapshot, cx);
                }
                self.sync_order_book_demand(cx);
                self.apply_order_book_to_chart(cx);
                cx.notify();
                true
            }
            Ok(false) => true,
            Err(_) => {
                Self::post_indicator_failure(
                    "order_flow",
                    "Order flow unavailable",
                    "The order-flow settings could not be applied.",
                    cx,
                );
                false
            }
        }
    }

    pub(super) fn cycle_time_sales_side_filter(&mut self, cx: &mut Context<Self>) {
        self.time_sales.filter.side = self.time_sales.filter.side.next();
        cx.notify();
    }

    pub(super) fn cycle_time_sales_size_filter(&mut self, cx: &mut Context<Self>) {
        self.time_sales.filter.minimum_quantity = match self.time_sales.filter.minimum_quantity {
            value if value < 1.0 => 1.0,
            value if value < 10.0 => 10.0,
            value if value < 100.0 => 100.0,
            _ => 0.0,
        };
        cx.notify();
    }

    pub(super) fn cycle_time_sales_price_filter(&mut self, cx: &mut Context<Self>) {
        self.time_sales.filter.price_range_ticks = match self.time_sales.filter.price_range_ticks {
            None => Some(10),
            Some(10) => Some(50),
            Some(_) => None,
        };
        cx.notify();
    }

    pub(super) fn reset_time_sales_filter(&mut self, cx: &mut Context<Self>) {
        if self.time_sales.filter.reset() {
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
            self.retained_chart_presentation.rejected_saved.appearance = false;
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
            self.retained_chart_presentation.rejected_saved.appearance = false;
            self.chart_persistence_dirty = true;
            cx.notify();
        }
    }

    pub(super) fn set_chart_axis_appearance(
        &mut self,
        axis: &ChartAxisAppearance,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_axis_appearance(axis);
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
            self.retained_chart_presentation.rejected_saved.appearance = false;
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
        self.retained_chart_presentation.rejected_saved.appearance = false;
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

    pub(super) fn set_chart_legend_options(
        &mut self,
        options: ChartLegendOptions,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = &self.chart else {
            return;
        };
        if chart.update(cx, |chart, chart_cx| {
            let changed = chart.set_legend_options(options);
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
        // Retain the replacement's tape, but do not even invalidate the old
        // chart's resume cursor when its prefix was rewritten. Installation of
        // the covering snapshot projects the retained tape into the new chart.
        if self.chart_pane_controller.chart_needs_replacement() {
            return;
        }
        let (Some(chart), Some(product)) = (self.chart.as_ref(), self.product.as_ref()) else {
            return;
        };
        let price_divisor = 10_f64.powi(i32::from(snapshot.price_scale));
        // Footprint rows are keyed by the instrument's price increment. Without a
        // provider-published increment the chart stays on candles; none is guessed.
        let tick_size = product
            .price_increment
            .and_then(|increment| {
                num_traits::ToPrimitive::to_f64(&increment)
                    .map(|increment| increment / price_divisor)
            })
            .filter(|tick| tick.is_finite() && *tick > 0.0);
        let identity = product.instrument_id.clone();
        let aggregation = order_flow_aggregation(self.interval);
        let rewritten = self
            .trade_tape
            .as_ref()
            .is_some_and(|prior| prior.rewrite_generation != snapshot.rewrite_generation);
        let resume_ordinal = chart.update(cx, |chart, _| {
            if rewritten {
                chart.invalidate_order_flow_prefix();
            }
            tick_size.and_then(|tick_size| {
                chart.order_flow_resume_ordinal(
                    &identity,
                    snapshot.provider_generation,
                    aggregation,
                    tick_size,
                )
            })
        });
        let start = if tick_size.is_some() {
            order_flow_projection_start(
                &snapshot.trades,
                resume_ordinal,
                SWEEP_CLASSIFICATION_WINDOW,
            )
        } else {
            snapshot
                .trades
                .len()
                .saturating_sub(SWEEP_CLASSIFICATION_WINDOW)
        };
        let Some(trades) = chart_order_flow_trades(snapshot, start) else {
            return;
        };
        let recent_start = trades.len().saturating_sub(SWEEP_CLASSIFICATION_WINDOW);
        self.trade_sweeps = classify_order_flow_sweeps(&trades[recent_start..]).into();
        let Some(tick_size) = tick_size else {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_order_flow_trades();
                chart_cx.notify();
            });
            Self::post_indicator_failure(
                "order_flow",
                "Order flow unavailable",
                "Footprint needs a published price increment for this market.",
                cx,
            );
            return;
        };
        let result = chart.update(cx, |chart, chart_cx| {
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
        if result.is_err() {
            Self::post_indicator_failure(
                "order_flow",
                "Order flow unavailable",
                "The current trade tape could not be applied to the chart.",
                cx,
            );
        } else {
            Self::clear_indicator_failure("order_flow", cx);
        }
    }

    pub(super) fn set_chart_type(&mut self, chart_type: ChartType, cx: &mut Context<Self>) {
        if !super::provider_chart_types(self.provider).contains(&chart_type) {
            Self::post_indicator_failure(
                "chart_type",
                "Chart settings unavailable",
                "This chart type needs trade prints that the selected market does not publish.",
                cx,
            );
            return;
        }
        Self::clear_indicator_failure("chart_type", cx);
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
        let persistence = self.lifecycle.persistence().clone();
        match persistence.request_chart_chrome_preferences_save(preferences) {
            Ok(true) => cx
                .background_executor()
                .spawn(async move {
                    if let Err(error) = persistence.run_chart_chrome_preferences_save_worker() {
                        diagnostic!("Aeris chart chrome could not be saved: {error}");
                    }
                })
                .detach(),
            Ok(false) => {}
            Err(error) => {
                diagnostic!("Aeris chart chrome could not be saved: {error}");
            }
        }
    }

    fn refresh_instrument_listing(&mut self, cx: &mut Context<Self>) {
        let query = self.symbol_input_query(cx);
        if !instrument_listing_refresh_needed(
            &self.symbol_browser,
            self.market_state.symbol_selection_pending,
            &query,
        ) {
            return;
        }
        // The search path posts its own keyed failure notice; a pending query
        // retains the latest request rather than reporting a second failure.
        self.search_symbol_query(&query, cx);
    }

    fn symbol_input_query(&self, cx: &App) -> String {
        self.symbol_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn post_symbol_search_notice(detail: &'static str, cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
            key: notification_center::NoticeKey {
                scope: cx.entity_id().as_u64(),
                kind: "symbol_search",
            },
            severity: notification_center::NoticeSeverity::Error,
            tone: notification_center::NoticeTone::Loss,
            source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
            title: "Symbol search unavailable",
            detail: detail.into(),
            action: None,
            lifetime: notification_center::NoticeLifetime::UntilCleared,
        }));
    }

    fn post_symbol_selection_notice(&self, detail: &str, cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
            key: notification_center::NoticeKey {
                scope: cx.entity_id().as_u64(),
                kind: "symbol_selection",
            },
            severity: notification_center::NoticeSeverity::Warning,
            tone: notification_center::NoticeTone::Warning,
            source: notification_center::NoticeSource::Provider(super::terminal_provider_id(
                self.symbol_provider,
            )),
            title: "Market selection unavailable",
            detail: detail.into(),
            action: None,
            lifetime: notification_center::NoticeLifetime::UntilCleared,
        }));
    }

    fn clear_symbol_selection_notice(cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Clear(notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "symbol_selection",
        }));
    }

    /// Derive normal menu status from accepted browser and chart state, not a second string.
    pub(super) fn symbol_selector_status(&self) -> String {
        if self.market_state.symbol_selection_pending {
            return "Selecting market".into();
        }
        if self.symbol_browser.has_retained_search() {
            return "Waiting to search the latest query".into();
        }
        if self.symbol_browser.search_pending() {
            return "Searching provider markets".into();
        }
        match self.symbol_browser.listing_summary() {
            Some(rithmic_shell::SymbolListingSummary::Products(count)) => {
                return format!(
                    "{} · expand one for its contracts",
                    counted(count, "product", "products")
                );
            }
            Some(rithmic_shell::SymbolListingSummary::Contracts {
                contracts,
                products,
            }) => {
                return format!(
                    "{} in {}",
                    counted(contracts, "contract", "contracts"),
                    counted(products, "product", "products")
                );
            }
            Some(rithmic_shell::SymbolListingSummary::Markets(count)) => {
                return format!("{count} matching markets · search to find more");
            }
            None => {}
        }
        if self.symbol_browser.has_completed_search() {
            return "No matching markets · try another search".into();
        }
        if self.symbol_provider == self.provider
            && self.chart_state == ChartState::Ready
            && let Some(product) = self.product.as_ref()
        {
            return super::provider_ready_message(self.provider, product);
        }
        super::provider_presentation(self.symbol_provider).map_or_else(
            || "Search provider markets".into(),
            |descriptor| descriptor.search_hint.into(),
        )
    }

    /// An empty query asks the provider for its own listing.
    fn search_symbol_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        if self.symbol_browser.search_pending() {
            if let Ok(already_dispatched) = self.symbol_browser.retain_latest_search(query) {
                cx.notify();
                return already_dispatched;
            }
            Self::post_symbol_search_notice(
                "Enter a market query of at most 64 printable bytes.",
                cx,
            );
            cx.notify();
            return false;
        }
        let Ok(request) = self.symbol_browser.begin_search(query) else {
            Self::post_symbol_search_notice(
                "Enter a market query of at most 64 printable bytes.",
                cx,
            );
            cx.notify();
            return false;
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
        let lane = request.lane;
        let search = SearchProviderInstruments {
            startup_resolve: false,
            consumer_id: 0,
            search_generation: u64::try_from(request_id.get()).unwrap_or(u64::MAX),
            provider: provider.to_string(),
            query: request.query,
            maximum_results: u32::try_from(self.symbol_browser.maximum_results())
                .unwrap_or(u32::MAX),
            categories: self.chart_chrome.symbol_search_categories,
        };
        let dispatched = if self.market_worker.try_search_provider(search).is_ok() {
            true
        } else {
            // A failed contract search collapses its product, so expanding it retries; a
            // typed query is kept for the next dispatch.
            self.symbol_browser.reject_search(request_id);
            if lane == rithmic_shell::SymbolSearchLane::Query
                && let Err(error) = self.symbol_browser.retain_latest_search(&retained_query)
            {
                // This query passed begin_search validation; the same keyed
                // notice below covers dispatch failure without exposing input.
                diagnostic!("Validated market search could not be retained: {error}");
            }
            Self::post_symbol_search_notice(
                "The market search could not complete. Try again later.",
                cx,
            );
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
            Self::post_symbol_search_notice(
                "The market search is unavailable. Try again later.",
                cx,
            );
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
        match symbol_submit_decision(entries.len(), self.chrome_selection) {
            SymbolSubmitDecision::Select(index) => entries.get(index).is_some_and(|entry| {
                self.activate_instrument_entry(&entry.action, self.symbol_selection_target, cx)
            }),
            SymbolSubmitDecision::Search => {
                self.search_symbol_input(cx);
                false
            }
        }
    }

    fn apply_catalog_event(&mut self, event: ProviderCatalogEvent, cx: &mut Context<Self>) {
        if let ProviderCatalogEvent::SelectionInstalled { instrument, .. } = &event {
            self.adopt_verified_terms(instrument);
        }
        if provider_catalog_event_provider(&event) != terminal_provider_id(self.symbol_provider) {
            return;
        }
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                self.apply_search_completed(result, cx);
            }
            ProviderCatalogEvent::SelectionInstalled {
                command_generation,
                instrument,
            } => {
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
                    Self::clear_symbol_selection_notice(cx);
                    cx.notify();
                }
            }
            ProviderCatalogEvent::CatalogReady { .. } => {
                if !self.market_state.catalog_listing_started {
                    self.market_state.catalog_listing_started = true;
                    // The search path owns dispatch failure feedback and queued
                    // replacement requests while catalog startup is pending.
                    let query = self.symbol_input_query(cx);
                    self.search_symbol_query(&query, cx);
                }
            }
            ProviderCatalogEvent::CommandRejected { rejection, command } => {
                self.apply_catalog_rejection(&rejection, command, cx);
            }
            // Chart surfaces never request market screens; the screener owns its own consumer.
            ProviderCatalogEvent::ScreenCompleted(_) | ProviderCatalogEvent::ScreenRejected(_) => {
                return;
            }
        }
        cx.notify();
    }

    /// The chart's provisional descriptor has no price increment, which the
    /// footprint and order tools need; the provider's verified terms replace it.
    fn adopt_verified_terms(&mut self, instrument: &InstallProviderInstrument) {
        if self.product.as_ref().is_some_and(|product| {
            product != instrument && verified_terms_supersede(product, instrument)
        }) {
            self.product = Some(instrument.clone());
            self.chart_persistence_dirty = true;
        }
    }

    fn apply_search_completed(
        &mut self,
        result: ProviderInstrumentSearchResult,
        cx: &mut Context<Self>,
    ) {
        if !self.apply_catalog_results(result.search_generation, result.instruments) {
            return;
        }
        cx.emit(PaneNoticeEvent::Clear(notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "symbol_search",
        }));
        if self.symbol_browser.has_retained_search() {
            self.dispatch_retained_symbol_search(cx);
            cx.notify();
            return;
        }
        if let Some(symbol) = self.pending_mnemonic_symbol.take() {
            let index = self.symbol_browser.results().iter().position(|instrument| {
                instrument.kind == ProviderInstrumentKind::Instrument
                    && (instrument.symbol.eq_ignore_ascii_case(&symbol)
                        || instrument.display_symbol.eq_ignore_ascii_case(&symbol))
            });
            if let Some(index) = index {
                let selection = InstrumentMenuSelection::SearchResult(index);
                if let Some(row) = self
                    .instrument_entries(cx)
                    .iter()
                    .position(|entry| entry.action == InstrumentMenuAction::Select(selection))
                {
                    self.chrome_selection = row;
                }
                self.select_instrument(selection, SymbolSelectionTarget::Chart, cx);
                return;
            }
            Self::post_symbol_search_notice("No exact market matched the requested symbol.", cx);
        }
        self.dispatch_retained_symbol_search(cx);
        if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
            if self.menu_state.chrome_list_keyboard
                && !self.symbol_input_query(cx).trim().is_empty()
            {
                self.chrome_selection =
                    first_selectable_instrument_index(&self.instrument_entries(cx)).unwrap_or(0);
            } else {
                self.sync_instrument_menu_keyboard(cx);
            }
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
        diagnostic!(
            "Market catalog {} rejected by {}: {:?}",
            if selection { "selection" } else { "search" },
            rejection.provider,
            rejection.reason
        );
        if selection {
            self.pending_symbol_selection_target = None;
            if self.chrome_overlay != Some(ChromeOverlay::Instrument)
                || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
            {
                self.symbol_selection_target = SymbolSelectionTarget::Chart;
            }
        }
        self.market_state.symbol_selection_pending = false;
        if selection {
            let detail = catalog_rejection_message(rejection.reason, command, self.provider);
            self.post_symbol_selection_notice(&detail, cx);
        } else {
            Self::post_symbol_search_notice(
                "The market search could not complete. Try again later.",
                cx,
            );
        }
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

    pub(super) fn toggle_order_entry(&mut self, cx: &mut Context<Self>) {
        self.toggle_side_panel(SidePanel::OrderEntry, cx);
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
        if panel == SidePanel::OrderBook && !visible {
            self.menu_state.order_book_column_open = false;
        }
        self.sync_order_book_demand(cx);
        self.chart_persistence_dirty = true;
        cx.notify();
    }

    /// Whether any consumer of this pane's live order-book frame is shown: a docked panel or
    /// the chart's right-edge book.
    fn order_book_demanded(&self, cx: &App) -> bool {
        self.side_panels.needs_order_book()
            || self.chart.as_ref().is_some_and(|chart| {
                chart
                    .read(cx)
                    .order_flow_settings()
                    .order_book_depth
                    .is_some_and(|book| book.visible)
            })
    }

    /// Opens or releases the pane's order-book demand when its consumers changed it. A request
    /// the worker could not take is sent again on the next change.
    pub(super) fn sync_order_book_demand(&mut self, cx: &mut Context<Self>) {
        let demanded = self.order_book_demanded(cx);
        let demand = if demanded {
            OrderBookDemand::Open
        } else {
            OrderBookDemand::Released
        };
        if demand == self.order_book_demand {
            return;
        }
        if demanded {
            self.order_book
                .update(cx, aeris_terminal_ui::ReadOnlyOrderBookView::clear);
        }
        let key = notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "order_book_demand",
        };
        if self
            .market_worker
            .try_set_order_book_visible(demanded)
            .is_err()
        {
            cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
                key,
                severity: notification_center::NoticeSeverity::Error,
                tone: notification_center::NoticeTone::Loss,
                source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
                title: "Order book unavailable",
                detail: "The order book request could not be sent. Try again later.".into(),
                action: None,
                lifetime: notification_center::NoticeLifetime::UntilCleared,
            }));
        } else {
            self.order_book_demand = demand;
            cx.emit(PaneNoticeEvent::Clear(key));
        }
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
            if let Err(error) = surface.update(cx, |surface, surface_cx| {
                surface.context_credential_message = Some(match result {
                    Ok(()) => "Keys saved; source refreshes were scheduled.".to_string(),
                    Err(error) => format!("Keys could not be saved: {error}"),
                });
                surface_cx.notify();
            }) && surface.upgrade().is_some()
            {
                diagnostic!("Live pane could not receive context credential save result: {error}");
            }
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
            // Terminal links financial charts only, which never publish general-chart events.
            aeris_chart_integration::ChartSyncEventKind::GeneralAxisView { .. }
            | aeris_chart_integration::ChartSyncEventKind::GeneralCrosshair { .. } => false,
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

    pub(super) fn select_drawing_tool(&mut self, tool: DrawingToolChoice, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                match tool {
                    DrawingToolChoice::Cursor => chart.set_drawing_tool(None),
                    DrawingToolChoice::Kind(kind) => chart.set_drawing_tool(Some(kind)),
                    DrawingToolChoice::Stamp(stamp) => chart.set_drawing_stamp(stamp),
                }
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
            Self::post_study_failure("Study settings are busy. Try again.", cx);
            return;
        }
        let Some(active) = self
            .studies
            .active
            .iter()
            .find(|state| state.study_id == study_id)
            .cloned()
        else {
            Self::post_study_failure("This study is no longer active.", cx);
            return;
        };
        let Ok(series) = self.current_runtime_series() else {
            Self::post_study_failure("Chart data is not available for this study yet.", cx);
            return;
        };
        let Ok(registration) =
            runtime_study_registration(&active.persisted, &series, &self.studies.active)
        else {
            Self::post_study_failure("Study settings could not be loaded.", cx);
            return;
        };
        let mut draft_values = BTreeMap::new();
        let mut inputs = HashMap::new();
        let mut subscriptions = Vec::new();
        let surface = cx.entity();
        for spec in &registration.definition.settings {
            let Some(value) = registration.settings.get(&spec.identifier).cloned() else {
                Self::post_study_failure("Study settings are incomplete.", cx);
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
        let saved_colors = persisted_study_output_colors(&active.persisted);
        let output_colors = self.chart.as_ref().map_or_else(Vec::new, |chart| {
            let chart = chart.read(cx);
            registration
                .definition
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, output)| matches!(output.plot, StudyPlotKind::Line))
                .filter_map(|(output_index, output)| {
                    Some(StudyOutputColorDraft {
                        output_index,
                        label: output.title.clone(),
                        color: saved_colors.get(output_index).cloned().flatten(),
                        default_color: chart
                            .study_output_default_color(study_id.get(), output_index)?,
                    })
                })
                .collect()
        });
        self.open_indicator_settings_dialog(
            IndicatorSettingsDialog::Study(StudySettingsDialogState {
                study_id,
                title: study_display_name(&active.persisted.identifier),
                specs: registration.definition.settings,
                draft_values,
                line_width: persisted_study_line_width(&active.persisted),
                output_colors,
                color_picker: None,
                inputs,
                _subscriptions: subscriptions,
                menu: None,
                message: None,
            }),
            cx,
        );
    }

    /// Replaces any open indicator settings dialog and dismisses the chrome menus.
    pub(super) fn open_indicator_settings_dialog(
        &mut self,
        dialog: IndicatorSettingsDialog,
        cx: &mut Context<Self>,
    ) {
        self.close_drawing_settings_dialog(false, cx);
        let kind = match &dialog {
            IndicatorSettingsDialog::Study(_) => "study",
            IndicatorSettingsDialog::Volume(_) => "volume_settings",
            IndicatorSettingsDialog::BigTrades(_) => "big_trades_settings",
            IndicatorSettingsDialog::OrderFlow(_) => "order_flow_settings",
            IndicatorSettingsDialog::Drawing(_) => "drawing",
        };
        self.indicator_settings_dialog = Some(dialog);
        Self::clear_indicator_failure(kind, cx);
        self.chrome_overlay = None;
        self.timeframe_menu_flyout = None;
        self.chrome_selection = 0;
        cx.notify();
    }

    pub(super) const fn study_settings_dialog(&self) -> Option<&StudySettingsDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Study(dialog)) => Some(dialog),
            _ => None,
        }
    }

    const fn study_settings_dialog_mut(&mut self) -> Option<&mut StudySettingsDialogState> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Study(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(super) const fn big_trades_dialog(&self) -> Option<&BigTradesDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::BigTrades(dialog)) => Some(dialog),
            _ => None,
        }
    }

    const fn big_trades_dialog_mut(&mut self) -> Option<&mut BigTradesDialogState> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::BigTrades(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(super) const fn volume_dialog(&self) -> Option<&VolumeDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Volume(dialog)) => Some(dialog),
            _ => None,
        }
    }

    const fn volume_dialog_mut(&mut self) -> Option<&mut VolumeDialogState> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Volume(dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// The open-dropdown slot of the indicator dialog; the drawing dialog keeps its own menus.
    const fn indicator_settings_menu_mut(&mut self) -> Option<&mut Option<String>> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Study(dialog)) => Some(&mut dialog.menu),
            Some(IndicatorSettingsDialog::BigTrades(dialog)) => Some(&mut dialog.menu),
            Some(IndicatorSettingsDialog::Volume(dialog)) => Some(&mut dialog.menu),
            Some(IndicatorSettingsDialog::OrderFlow(dialog)) => Some(&mut dialog.menu),
            Some(IndicatorSettingsDialog::Drawing(_)) | None => None,
        }
    }

    /// Opens a setting's dropdown, closing any other dropdown or color picker, or closes it
    /// when it is already open.
    pub(super) fn toggle_indicator_settings_menu(&mut self, menu: &str, cx: &mut Context<Self>) {
        if let Some(dialog) = self.volume_dialog_mut() {
            dialog.color_picker = None;
        }
        if let Some(dialog) = self.study_settings_dialog_mut() {
            dialog.color_picker = None;
        }
        if let Some(dialog) = self.order_flow_dialog_mut() {
            dialog.color_picker = None;
        }
        if let Some(open) = self.indicator_settings_menu_mut() {
            *open = (open.as_deref() != Some(menu)).then(|| menu.to_string());
            cx.notify();
        }
    }

    pub(super) fn close_indicator_settings_menu(&mut self, cx: &mut Context<Self>) {
        if self
            .indicator_settings_menu_mut()
            .is_some_and(|open| open.take().is_some())
        {
            cx.notify();
        }
    }

    pub(super) fn set_study_settings_line_width(&mut self, width: u8, cx: &mut Context<Self>) {
        if let Some(dialog) = self.study_settings_dialog_mut()
            && dialog.line_width != width
        {
            dialog.line_width = width;
            cx.notify();
        }
    }

    pub(super) fn toggle_study_dialog_color_picker(
        &mut self,
        output_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.study_settings_dialog_mut() else {
            return;
        };
        dialog.menu = None;
        if dialog
            .color_picker
            .as_ref()
            .is_some_and(|picker| picker.target == output_index)
        {
            dialog.color_picker = None;
        } else if let Some(draft) = dialog
            .output_colors
            .iter()
            .find(|draft| draft.output_index == output_index)
        {
            let current = draft.effective_color().to_ascii_uppercase();
            let input = cx.new(|input_cx| InputState::new(window, input_cx));
            input.update(cx, |input, input_cx| {
                input.set_value(current, window, input_cx);
            });
            dialog.color_picker = Some(DialogColorPickerState {
                target: output_index,
                input,
                error: None,
            });
        }
        cx.notify();
    }

    pub(super) fn close_study_dialog_color_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.study_settings_dialog_mut()
            && dialog.color_picker.take().is_some()
        {
            cx.notify();
        }
    }

    pub(super) fn set_study_dialog_color(
        &mut self,
        output_index: usize,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.study_settings_dialog_mut() else {
            return;
        };
        let normalized = normalize_hex_color(color);
        if let Some(picker) = &mut dialog.color_picker
            && picker.target == output_index
        {
            picker.error = normalized
                .is_none()
                .then(|| "Enter a hex color such as #2962FF or #2962FF80.".to_string());
        }
        if let Some(color) = normalized
            && let Some(draft) = dialog
                .output_colors
                .iter_mut()
                .find(|draft| draft.output_index == output_index)
        {
            draft.color = (!color.eq_ignore_ascii_case(&draft.default_color)).then_some(color);
        }
        cx.notify();
    }

    /// Applies a study's stroke width and line colors to the chart and its durable state.
    fn apply_study_style(
        &mut self,
        study_id: StudyInstanceId,
        width: u8,
        output_colors: &[String],
        cx: &mut Context<Self>,
    ) {
        if let Some(active) = self
            .studies
            .active
            .iter_mut()
            .find(|state| state.study_id == study_id)
            && (active.persisted.line_width != u32::from(width)
                || active.persisted.output_colors != output_colors)
        {
            active.persisted.line_width = u32::from(width);
            active.persisted.output_colors = output_colors.to_vec();
            self.chart_persistence_dirty = true;
        }
        let colors = output_colors
            .iter()
            .map(|color| (!color.is_empty()).then(|| color.clone()))
            .collect::<Vec<_>>();
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let width_changed = chart.set_study_line_width(study_id.get(), width);
                if chart.set_study_output_colors(study_id.get(), &colors) || width_changed {
                    chart_cx.notify();
                }
            });
        }
    }

    pub(super) fn close_study_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if self.study_settings_dialog().is_some() {
            self.indicator_settings_dialog = None;
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
            Self::post_indicator_failure(
                "study",
                "Study unavailable",
                "This study is no longer active.",
                cx,
            );
            return;
        }
        match self.market_worker.try_remove_study(study_id) {
            Ok(()) => {
                self.studies.removing.insert(study_id);
                if self
                    .study_settings_dialog()
                    .is_some_and(|dialog| dialog.study_id == study_id)
                {
                    self.indicator_settings_dialog = None;
                }
                Self::clear_indicator_failure("study", cx);
            }
            Err(_) => {
                Self::post_indicator_failure(
                    "study",
                    "Study unavailable",
                    "The study could not be removed. Try again.",
                    cx,
                );
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
        if let Some(dialog) = self.study_settings_dialog_mut() {
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
        if let Some(dialog) = self.study_settings_dialog_mut() {
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
        let Some(IndicatorSettingsDialog::Study(dialog)) = &mut self.indicator_settings_dialog
        else {
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
        for draft in &mut dialog.output_colors {
            draft.color = None;
        }
        dialog.color_picker = None;
        dialog.message = None;
        cx.notify();
    }

    pub(super) fn save_study_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.study_settings_dialog() else {
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
        let output_colors = persisted_output_colors(
            active.persisted.output_identifiers.len(),
            &dialog.output_colors,
        );
        self.apply_study_style(study_id, line_width, &output_colors, cx);
        let active_settings = active.persisted.settings.clone();
        let mut replacement = active.persisted;
        replacement.line_width = u32::from(line_width);
        replacement.output_colors = output_colors;
        replacement.settings = values
            .iter()
            .map(|(identifier, value)| WorkspaceStudySettingState {
                identifier: identifier.clone(),
                value: Some(persisted_study_setting_value(value)),
            })
            .collect();
        if replacement.settings == active_settings {
            // Only presentation changed; the runtime computation stays as it is.
            self.indicator_settings_dialog = None;
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
        if let Some(dialog) = self.study_settings_dialog_mut() {
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
            Self::post_indicator_failure(
                "big_trades_settings",
                "Order flow unavailable",
                "Big trades is no longer on this chart.",
                cx,
            );
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
        self.open_indicator_settings_dialog(
            IndicatorSettingsDialog::BigTrades(BigTradesDialogState {
                intensity,
                size: settings.size,
                show_volume: settings.show_volume,
                minimum_volume,
                menu: None,
                message: None,
            }),
            cx,
        );
    }

    pub(super) fn close_big_trades_dialog(&mut self, cx: &mut Context<Self>) {
        if self.big_trades_dialog().is_some() {
            self.indicator_settings_dialog = None;
            cx.notify();
        }
    }

    pub(super) fn set_big_trades_dialog_filter(
        &mut self,
        intensity: Option<BigTradesIntensity>,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.big_trades_dialog_mut()
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
        if let Some(dialog) = self.big_trades_dialog_mut()
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
        if let Some(dialog) = self.big_trades_dialog_mut()
            && dialog.show_volume != show_volume
        {
            dialog.show_volume = show_volume;
            cx.notify();
        }
    }

    pub(super) fn reset_big_trades_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.big_trades_dialog_mut() else {
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
        let Some(dialog) = self.big_trades_dialog() else {
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
        self.indicator_settings_dialog = None;
        self.set_chart_order_flow_settings(settings, cx);
        cx.notify();
    }

    fn show_big_trades_message(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.big_trades_dialog_mut() {
            dialog.message = Some(message);
        }
        cx.notify();
    }

    /// Opens a draft of the chart's volume settings.
    pub(super) fn open_volume_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = self
            .chart
            .as_ref()
            .map(|chart| chart.read(cx).volume_settings())
        else {
            return;
        };
        let Some(settings) = settings else {
            Self::post_indicator_failure(
                "volume_settings",
                "Study unavailable",
                "Volume is no longer on this chart.",
                cx,
            );
            return;
        };
        let average_length =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Bars"));
        average_length.update(cx, |input, input_cx| {
            input.set_value(settings.average.length.to_string(), window, input_cx);
        });
        self.open_indicator_settings_dialog(
            IndicatorSettingsDialog::Volume(VolumeDialogState {
                draft: settings,
                average_length,
                color_picker: None,
                menu: None,
                message: None,
            }),
            cx,
        );
    }

    pub(super) fn close_volume_dialog(&mut self, cx: &mut Context<Self>) {
        if self.volume_dialog().is_some() {
            self.indicator_settings_dialog = None;
            cx.notify();
        }
    }

    pub(super) fn set_volume_dialog_average_enabled(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.volume_dialog_mut()
            && dialog.draft.average.enabled != enabled
        {
            dialog.draft.average.enabled = enabled;
            dialog.message = None;
            if !enabled
                && dialog
                    .color_picker
                    .as_ref()
                    .is_some_and(|picker| picker.target == VolumeColorTarget::Average)
            {
                dialog.color_picker = None;
            }
            cx.notify();
        }
    }

    pub(super) fn set_volume_dialog_average_kind(
        &mut self,
        kind: VolumeAverageKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.volume_dialog_mut()
            && dialog.draft.average.kind != kind
        {
            dialog.draft.average.kind = kind;
            cx.notify();
        }
    }

    pub(super) fn set_volume_dialog_placement(
        &mut self,
        placement: VolumePlacement,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.volume_dialog_mut()
            && dialog.draft.placement != placement
        {
            dialog.draft.placement = placement;
            cx.notify();
        }
    }

    pub(super) fn toggle_volume_dialog_color_picker(
        &mut self,
        target: VolumeColorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.volume_dialog_mut() else {
            return;
        };
        dialog.menu = None;
        if dialog
            .color_picker
            .as_ref()
            .is_some_and(|picker| picker.target == target)
        {
            dialog.color_picker = None;
        } else {
            let current = target.color(&dialog.draft);
            let input = cx.new(|input_cx| InputState::new(window, input_cx));
            input.update(cx, |input, input_cx| {
                input.set_value(current.to_ascii_uppercase(), window, input_cx);
            });
            dialog.color_picker = Some(DialogColorPickerState {
                target,
                input,
                error: None,
            });
        }
        cx.notify();
    }

    pub(super) fn close_volume_dialog_color_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.volume_dialog_mut()
            && dialog.color_picker.take().is_some()
        {
            cx.notify();
        }
    }

    pub(super) fn set_volume_dialog_color(
        &mut self,
        target: VolumeColorTarget,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.volume_dialog_mut() else {
            return;
        };
        let normalized = normalize_hex_color(color);
        if let Some(picker) = &mut dialog.color_picker
            && picker.target == target
        {
            picker.error = normalized
                .is_none()
                .then(|| "Enter a hex color such as #2962FF or #2962FF80.".to_string());
        }
        if let Some(color) = normalized {
            let slot = match target {
                VolumeColorTarget::Up => &mut dialog.draft.up_color,
                VolumeColorTarget::Down => &mut dialog.draft.down_color,
                VolumeColorTarget::Average => &mut dialog.draft.average.color,
            };
            *slot = Some(color);
        }
        cx.notify();
    }

    pub(super) fn reset_volume_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.volume_dialog_mut() else {
            return;
        };
        dialog.draft = VolumeSettings::default();
        dialog.average_length.update(cx, |input, input_cx| {
            input.set_value(
                VolumeSettings::default().average.length.to_string(),
                window,
                input_cx,
            );
        });
        dialog.color_picker = None;
        dialog.message = None;
        cx.notify();
    }

    /// Applies the draft through the chart, which rebuilds the Aeris Charts volume indicator.
    /// Visibility stays with the legend.
    pub(super) fn apply_volume_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.volume_dialog() else {
            return;
        };
        let length =
            match parse_volume_average_length(dialog.average_length.read(cx).value().as_ref()) {
                Ok(length) => length,
                Err(message) => return self.show_volume_message(message, cx),
            };
        let mut settings = dialog.draft.clone();
        settings.average.length = length;
        let Some(chart) = &self.chart else {
            return self.show_volume_message("The chart is unavailable".to_string(), cx);
        };
        let applied = chart.update(cx, |chart, chart_cx| {
            if chart.volume_settings().is_none() {
                return Err("Volume is no longer on this chart".to_string());
            }
            if chart.volume_settings().as_ref() == Some(&settings) {
                return Ok(());
            }
            if !chart.set_volume_settings(settings) {
                return Err("The chart could not apply these settings".to_string());
            }
            chart_cx.notify();
            Ok(())
        });
        match applied {
            Ok(()) => {
                self.indicator_settings_dialog = None;
                self.chart_persistence_dirty = true;
                cx.notify();
            }
            Err(message) => self.show_volume_message(message, cx),
        }
    }

    fn show_volume_message(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.volume_dialog_mut() {
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
                    Self::post_indicator_failure(
                        "study",
                        "Study unavailable",
                        "Some studies could not be removed. Try again.",
                        cx,
                    );
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
            Self::post_study_failure("Chart data is not available yet.", cx);
            return false;
        };
        let maximum = current_plan_limits().indicators_per_chart;
        let runtime_count = runtime_study_count(&self.studies);
        if chart.read(cx).indicator_states().len() + runtime_count >= maximum {
            Self::post_study_failure("This chart has reached its indicator limit.", cx);
            return false;
        }
        if runtime_managed_indicator(indicator) {
            if self.current_runtime_series().is_err() {
                Self::post_study_failure("Chart data is not available for this study yet.", cx);
                return false;
            }
            let Some(local_id) = self.studies.allocate_local_id() else {
                Self::post_study_failure("Study identity space is exhausted.", cx);
                return false;
            };
            let Some(persisted) = legacy_runtime_study(local_id, indicator, true) else {
                Self::post_study_failure("Study implementation is unavailable.", cx);
                return false;
            };
            let state = PendingRuntimeStudyState {
                persisted,
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            };
            if self.enqueue_runtime_study(state).is_err() {
                Self::post_study_failure("The study could not be queued. Try again.", cx);
                return false;
            }
            Self::clear_indicator_failure("study", cx);
            self.chart_persistence_dirty = true;
            cx.notify();
            return true;
        }
        let result = chart.update(cx, |chart, chart_cx| {
            let result = chart.add_indicator(indicator);
            if result.is_ok() {
                chart_cx.notify();
            }
            result
        });
        if result.is_err() {
            Self::post_study_failure("The indicator could not be added.", cx);
            return false;
        }
        Self::clear_indicator_failure("study", cx);
        if indicator == ChartIndicator::Volume {
            Self::clear_indicator_failure("volume_settings", cx);
        }
        self.retain_chart_presentation(cx);
        true
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
