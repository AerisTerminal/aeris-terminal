//! Persisted workspace schema: the workspace file's messages, including the instrument and
//! series identities it stores.

/// Persisted split direction for one workspace pane grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum WorkspaceSplitAxis {
    Horizontal = 0,
    Vertical = 1,
}

/// Persisted terminal component kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum WorkspacePaneKind {
    Chart = 0,
    OrderBook = 1,
    Watchlist = 2,
    OrderEntry = 3,
}

/// Canonical cadence carried by a provider-neutral bar-series identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum SeriesCadence {
    /// Invalid or absent cadence.
    Unspecified = 0,
    /// Fixed intraday seconds.
    FixedSeconds = 1,
    /// Trades per bar.
    Trades = 2,
    /// Exchange-session days per bar.
    SessionDays = 3,
    /// Calendar weeks per bar.
    CalendarWeeks = 4,
    /// Calendar months per bar.
    CalendarMonths = 5,
}

/// Durable comparison applied to an observed trade price.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum PriceAlertCondition {
    Crossing = 0,
    CrossingUp = 1,
    CrossingDown = 2,
    GreaterThan = 3,
    LessThan = 4,
}

/// Durable trigger policy for a price alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum PriceAlertFrequency {
    OnlyOnce = 0,
    EveryTime = 1,
}

/// Durable lifecycle state for a price alert line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum PriceAlertStatus {
    Active = 0,
    Triggered = 1,
}

/// Durable source kind for one native study dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum WorkspaceStudyDependencyKind {
    Unspecified = 0,
    CurrentChartSeries = 1,
    ExplicitSeries = 2,
    StudyOutput = 3,
}

/// Durable market stream requested by one native study market dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum WorkspaceStudyMarketStream {
    Unspecified = 0,
    Bars = 1,
    Trades = 2,
    Quotes = 3,
    Depth = 4,
}

/// Persisted workspace state.
#[derive(Clone, PartialEq, prost::Message)]
pub struct WorkspaceState {
    /// Active provider.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Active market.
    #[prost(string, tag = "2")]
    pub market: String,
    /// Active chart interval seconds.
    #[prost(uint32, tag = "3")]
    pub interval_seconds: u32,
    /// Watched markets.
    #[prost(string, repeated, tag = "4")]
    pub watchlist: Vec<String>,
    /// Workspace revision.
    #[prost(uint64, tag = "5")]
    pub workspace_revision: u64,
    /// Workspace schema revision.
    #[prost(uint32, tag = "8")]
    pub schema_revision: u32,
    // Tags 6, 7, and 9-13 are permanently retired. They described the deleted
    // former process-lifetime, resource-mode, and hot-history-cache.
    /// Monotonic generation of user-authored workspace composition.
    #[prost(uint64, tag = "14")]
    pub layout_generation: u64,
    /// Selected workspace identity.
    #[prost(uint64, tag = "15")]
    pub active_workspace_id: u64,
    /// Bounded deterministic workspace/tab/pane composition.
    #[prost(message, repeated, tag = "16")]
    pub workspace_tabs: Vec<WorkspaceTabState>,
    /// Bounded user-named chart appearance templates shared by every workspace.
    #[prost(message, repeated, tag = "17")]
    pub chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    /// Optional user-selected reset/default appearance.
    #[prost(message, optional, tag = "18")]
    pub default_chart_settings: Option<WorkspaceChartSettingsTemplateState>,
    /// Durable provider-neutral instruments shown in the native watchlist.
    #[prost(message, repeated, tag = "19")]
    pub watchlist_entries: Vec<WorkspaceWatchlistEntryState>,
    /// Bounded user-named drawing style templates shared by every chart.
    #[prost(message, repeated, tag = "20")]
    pub drawing_templates: Vec<WorkspaceDrawingTemplateState>,
    /// Bounded library of user-named saved layouts; every workspace tab shows one of them.
    #[prost(message, repeated, tag = "21")]
    pub saved_layouts: Vec<WorkspaceSavedLayoutState>,
    /// Durable shell chrome preferences. Absent before schema 4 folds the legacy
    /// chart-chrome key=value file into the document.
    #[prost(message, optional, tag = "22")]
    pub chart_chrome: Option<WorkspaceChromeState>,
}

/// Durable shell chrome that follows the user across charts and workspaces. Enum values are
/// the same stable identifiers the legacy chart-chrome key=value file carried, so the
/// schema 4 fold copies them verbatim and unknown identifiers fall back to the defaults.
#[allow(clippy::struct_excessive_bools)] // Protobuf mirrors independent persisted UI toggles.
#[derive(Clone, PartialEq, prost::Message)]
pub struct WorkspaceChromeState {
    #[prost(bool, tag = "1")]
    pub indicator_name_labels_visible: bool,
    #[prost(bool, tag = "2")]
    pub indicator_value_labels_visible: bool,
    #[prost(bool, tag = "3")]
    pub indicator_price_lines_visible: bool,
    /// The Aeris Charts chart type identifier, for example `candles`.
    #[prost(string, tag = "4")]
    pub chart_type: String,
    /// Instrument categories the symbol menu searches.
    #[prost(bool, tag = "5")]
    pub symbol_search_futures: bool,
    #[prost(bool, tag = "6")]
    pub symbol_search_equities: bool,
    /// `all`, `provider:<id>` or `exchange:<id>:<code>`.
    #[prost(string, tag = "7")]
    pub symbol_search_scope: String,
    /// `framed` or `frameless`.
    #[prost(string, tag = "8")]
    pub window_frame: String,
    /// The drawing tools sidebar along the left edge of the charts.
    #[prost(bool, tag = "9")]
    pub drawing_toolbar_visible: bool,
    /// Comma-separated drawing kind names and `stamp:` keys, encoded as the legacy file did;
    /// unknown names are skipped so a newer document never fails to load.
    #[prost(string, tag = "10")]
    pub drawing_favorites: String,
    #[prost(bool, tag = "11")]
    pub drawing_favorites_toolbar_visible: bool,
    /// Window-relative logical origin the favorites toolbar was dragged to.
    #[prost(message, optional, tag = "12")]
    pub drawing_favorites_origin: Option<WorkspaceChromePoint>,
    /// `confirm` or `one_click`.
    #[prost(string, tag = "13")]
    pub trading_shortcuts: String,
    /// The Buy / quantity / Sell bar under each chart's symbol legend.
    #[prost(bool, tag = "14")]
    pub chart_trade_buttons_visible: bool,
    /// Window-relative logical origin the selected-drawing toolbar was dragged to.
    #[prost(message, optional, tag = "15")]
    pub drawing_context_toolbar_origin: Option<WorkspaceChromePoint>,
}

/// A window-relative logical position in the chrome preferences.
#[derive(Clone, Copy, PartialEq, prost::Message)]
pub struct WorkspaceChromePoint {
    #[prost(float, tag = "1")]
    pub x: f32,
    #[prost(float, tag = "2")]
    pub y: f32,
}

/// One user-named saved layout: a pane arrangement plus the drawings of every instrument
/// shown on it, shared by every tab and pane that shows the layout.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceSavedLayoutState {
    /// Stable non-zero identity referenced by `WorkspaceTabState::saved_layout_id`.
    #[prost(uint64, tag = "1")]
    pub layout_id: u64,
    #[prost(string, tag = "2")]
    pub name: String,
    /// Whether every change is written to the saved layout. Off keeps changes in the working
    /// state until an explicit save.
    #[prost(bool, tag = "3")]
    pub autosave: bool,
    #[prost(int64, tag = "4")]
    pub saved_at_unix_ms: i64,
    #[prost(int64, tag = "5")]
    pub last_opened_unix_ms: i64,
    /// Saved pane arrangement. Its panes never carry drawings; those live in tag 7.
    #[prost(message, optional, tag = "6")]
    pub tab: Option<WorkspaceTabState>,
    /// Saved drawings of every instrument shown on the layout, most recently used first.
    #[prost(message, repeated, tag = "7")]
    pub instrument_drawings: Vec<WorkspaceInstrumentDrawingsState>,
}

/// One user-named drawing style template: the Aeris Charts template document of one drawing
/// kind. Aeris Charts validates the document and applies only its style keys.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceDrawingTemplateState {
    /// The Aeris Charts drawing kind wire name, for example `trend_line`.
    #[prost(string, tag = "1")]
    pub kind: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(string, tag = "3")]
    pub template_json: String,
}

/// One durable native watchlist row. Live prices remain runtime-owned and are
/// deliberately rebuilt from provider state after restart.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceWatchlistEntryState {
    #[prost(message, optional, tag = "1")]
    pub instrument: Option<InstallProviderInstrument>,
    /// The row's flag as a CSS hex color such as `#335CFF`. Empty means unflagged, which
    /// is also how entries written before flags existed decode.
    #[prost(string, tag = "2")]
    pub flag_color: String,
}

/// One locally persisted chart styling template. The chart type, market data, indicators,
/// drawings, and scale state are deliberately excluded. Tag 2 held the chart type in older
/// templates; it is ignored on decode and must not be reused.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartSettingsTemplateState {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, optional, tag = "3")]
    pub appearance: Option<WorkspaceChartAppearanceState>,
    #[prost(uint32, tag = "4")]
    pub crosshair_mode: u32,
    /// Persisted IANA display time zone. Empty in legacy workspaces means Etc/UTC.
    #[prost(string, tag = "5")]
    pub time_zone: String,
    /// Whether host-projected position/order management lines are visible. Absent means visible
    /// for legacy templates.
    #[prost(bool, optional, tag = "6")]
    pub show_order_management_lines: Option<bool>,
    /// Whether host-projected execution/fill markers are visible. Absent means visible for legacy
    /// templates.
    #[prost(bool, optional, tag = "7")]
    pub show_execution_marks: Option<bool>,
    /// Whether order and position lines extend to the pane's left edge. Absent means enabled.
    #[prost(bool, optional, tag = "8")]
    pub extend_order_lines_left: Option<bool>,
    /// Which parts of the chart legend show. Absent in legacy templates means all shown.
    #[prost(message, optional, tag = "9")]
    pub legend: Option<WorkspaceChartLegendState>,
}

/// Persisted presentation state for one pane.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspacePaneState {
    #[prost(uint64, tag = "1")]
    pub pane_id: u64,
    #[prost(uint64, tag = "2")]
    pub consumer_id: u64,
    #[prost(enumeration = "WorkspacePaneKind", tag = "3")]
    pub kind: i32,
    #[prost(message, optional, tag = "4")]
    pub instrument: Option<InstallProviderInstrument>,
    #[prost(message, optional, tag = "5")]
    pub series: Option<SeriesKey>,
    #[prost(sint64, optional, tag = "6")]
    pub viewport_start_unix_nanos: Option<i64>,
    #[prost(sint64, optional, tag = "7")]
    pub viewport_end_unix_nanos: Option<i64>,
    /// Normalized pane size; all panes in a tab sum to 10,000.
    #[prost(uint32, tag = "8")]
    pub size_basis_points: u32,
    /// Per-pane selection/layout generation.
    #[prost(uint64, tag = "9")]
    pub generation: u64,
    /// Durable user-authored chart presentation and drawing state.
    #[prost(message, optional, tag = "10")]
    pub chart: Option<WorkspaceChartState>,
    /// Visible native side-panel components: bit 0 order book, bit 1 watchlist,
    /// bit 2 Time & Sales, bit 3 order entry.
    #[prost(uint32, tag = "11")]
    pub side_panel_visibility: u32,
    /// Side-panel width in logical pixels.
    #[prost(uint32, tag = "12")]
    pub side_panel_width: u32,
    /// Stable `ChartInterval` label. Empty means the legacy `series` cadence must be read.
    #[prost(string, tag = "14")]
    pub interval: String,
    /// Order-book share when both side-panel components are visible.
    #[prost(uint32, tag = "13")]
    pub side_panel_split_basis_points: u32,
}

/// Durable chart state owned by one workspace pane.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartState {
    /// Stable `Aeris` chart-type identifier.
    #[prost(string, tag = "1")]
    pub chart_type: String,
    /// Drawings of the pane's instrument in workspaces saved before
    /// `instrument_drawings` existed. New saves leave it empty.
    #[prost(string, tag = "2")]
    pub chart_state_json: String,
    /// Host-owned native indicators in stable creation order.
    #[prost(message, repeated, tag = "3")]
    pub indicators: Vec<WorkspaceChartIndicatorState>,
    /// Stable product price-axis customization.
    #[prost(message, optional, tag = "4")]
    pub price_axis: Option<WorkspacePriceAxisState>,
    /// Locks of the legacy `chart_state_json` drawings, keyed by their saved id.
    #[prost(uint32, repeated, tag = "5")]
    pub locked_drawing_ids: Vec<u32>,
    /// Stable Aeris Charts crosshair mode (normal, magnet, hidden, or OHLC magnet).
    #[prost(uint32, tag = "6")]
    pub crosshair_mode: u32,
    /// Bounded host-owned price alerts. Aeris Charts renders these values but does
    /// not evaluate them or deliver operating-system notifications.
    #[prost(message, repeated, tag = "7")]
    pub price_alerts: Vec<WorkspacePriceAlertState>,
    /// Host-owned native study graph in stable creation/dependency order.
    #[prost(message, repeated, tag = "8")]
    pub studies: Vec<WorkspaceChartStudyState>,
    /// Host-owned durable chart appearance. Aeris Charts remains the renderer/state
    /// executor; this stores only product-authored presentation preferences.
    #[prost(message, optional, tag = "9")]
    pub appearance: Option<WorkspaceChartAppearanceState>,
    /// Footprint/CVD/delta and big-trades presentation. The retained trade
    /// tape itself remains runtime-owned and is never persisted here.
    #[prost(message, optional, tag = "10")]
    pub order_flow: Option<WorkspaceOrderFlowSettingsState>,
    /// Host-owned bottom context-panel visibility.
    #[prost(bool, tag = "11")]
    pub context_panel_visible: bool,
    /// Stable host-owned context-panel tab identifier.
    #[prost(uint32, tag = "12")]
    pub context_panel_tab: u32,
    /// Zero disables linking; 1-4 identify bounded desktop link groups A-D.
    #[prost(uint32, tag = "13")]
    pub chart_link_group: u32,
    /// Bit 0 synchronizes crosshairs and bit 1 synchronizes visible time ranges.
    #[prost(uint32, tag = "14")]
    pub chart_link_flags: u32,
    /// Logical-pixel height of the bottom context panel; zero (and absent in
    /// older workspaces) uses the default height.
    #[prost(uint32, tag = "15")]
    pub context_panel_height: u32,
    /// Persisted IANA display time zone. Empty in legacy workspaces means Etc/UTC.
    #[prost(string, tag = "16")]
    pub time_zone: String,
    /// Host-only chart trading presentation. Absent in legacy workspaces means visible.
    #[prost(bool, optional, tag = "17")]
    pub show_order_management_lines: Option<bool>,
    /// Host-only execution/fill marker presentation. Absent in legacy workspaces means visible.
    #[prost(bool, optional, tag = "18")]
    pub show_execution_marks: Option<bool>,
    /// Chart order and position line extension. Absent in legacy workspaces means enabled.
    #[prost(bool, optional, tag = "19")]
    pub extend_order_lines_left: Option<bool>,
    /// Which parts of the chart legend show. Absent in legacy workspaces means all shown.
    #[prost(message, optional, tag = "20")]
    pub legend: Option<WorkspaceChartLegendState>,
    /// Drawings of each instrument the pane showed in workspaces saved before saved layouts
    /// owned drawings. Read only to migrate them into the layout; new saves leave it empty.
    #[prost(message, repeated, tag = "21")]
    pub instrument_drawings: Vec<WorkspaceInstrumentDrawingsState>,
    /// Heights the user gave separate chart panes, by pane content. Empty (and absent in older
    /// workspaces) keeps the chart's default pane heights.
    #[prost(message, repeated, tag = "22")]
    pub pane_heights: Vec<WorkspaceChartPaneHeightState>,
}

/// The height of one separate chart pane, kept by what the pane shows so a rebuilt chart
/// restores it.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartPaneHeightState {
    /// Stable chart-integration pane content identifier, such as `volume` or `study.3`.
    #[prost(string, tag = "1")]
    pub content: String,
    /// The pane's height as a multiple of the main price pane's height, in basis points.
    #[prost(uint32, tag = "2")]
    pub share_basis_points: u32,
}

/// Committed drawings of one instrument, keyed by its stable provider identity so
/// price-anchored drawings never follow a symbol switch onto another market.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceInstrumentDrawingsState {
    #[prost(string, tag = "1")]
    pub provider: String,
    #[prost(string, tag = "2")]
    pub instrument_id: String,
    /// Bounded Aeris Charts semantic drawing document. It contains committed
    /// drawings only, never market history, series, indicator definitions, or
    /// renderer/runtime caches.
    #[prost(string, tag = "3")]
    pub chart_state_json: String,
    /// Locks of workspaces saved before locks were drawing state, keyed by saved drawing id.
    #[prost(uint32, repeated, tag = "4")]
    pub locked_drawing_ids: Vec<u32>,
}

/// Chart legend presentation. Every absent field means shown.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartLegendState {
    /// The symbol row.
    #[prost(bool, optional, tag = "1")]
    pub show_symbol: Option<bool>,
    /// The symbol row's open, high, low and close readouts.
    #[prost(bool, optional, tag = "2")]
    pub show_symbol_values: Option<bool>,
    /// The rows of indicators, studies and order-flow tools.
    #[prost(bool, optional, tag = "3")]
    pub show_indicators: Option<bool>,
    /// The readouts on those rows.
    #[prost(bool, optional, tag = "4")]
    pub show_indicator_values: Option<bool>,
}

/// Tags 4 and 5 held the retired footprint trade-bubble toggle and threshold; never reuse them.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceOrderFlowSettingsState {
    #[prost(uint32, tag = "1")]
    pub display_mode: u32,
    #[prost(bool, tag = "2")]
    pub show_cumulative_delta: bool,
    #[prost(bool, tag = "3")]
    pub show_delta_histogram: bool,
    /// Instrument ticks per footprint row; zero (and absent in older workspaces) is automatic.
    #[prost(uint32, tag = "6")]
    pub ticks_per_row: u32,
    /// Revision 1 records study visibility after CVD and Delta became opt-in indicators.
    /// Older workspaces omit this field and are migrated away from the former default-on panes.
    #[prost(uint32, tag = "7")]
    pub study_visibility_revision: u32,
    /// The big-trades indicator; absent when it is not added, including every workspace saved
    /// before it replaced the footprint trade bubbles.
    #[prost(message, optional, tag = "8")]
    pub big_trades: Option<WorkspaceBigTradesState>,
    /// The current-book bars at the price pane's right edge; absent when the indicator is not
    /// added, otherwise whether it is shown.
    #[prost(bool, optional, tag = "9")]
    pub order_book_depth: Option<bool>,
    /// 0 draws CVD as a line and 1 as candles; absent in older workspaces, which drew lines.
    #[prost(uint32, tag = "10")]
    pub cumulative_delta_style: u32,
    /// CVD reset and colors; absent in older workspaces, which reset each session in the
    /// default colors.
    #[prost(message, optional, tag = "11")]
    pub cumulative_delta: Option<WorkspaceCumulativeDeltaState>,
    /// Delta histogram colors; absent in older workspaces, which drew the default colors.
    #[prost(message, optional, tag = "12")]
    pub delta_histogram: Option<WorkspaceDeltaHistogramState>,
    /// Order-book bar options while tag 9 records the indicator; absent in older workspaces,
    /// which drew the default bars.
    #[prost(message, optional, tag = "13")]
    pub order_book_depth_options: Option<WorkspaceOrderBookDepthState>,
    /// The depth heatmap; absent when the indicator is not added, otherwise whether it is shown.
    #[prost(bool, optional, tag = "14")]
    pub depth_heatmap: Option<bool>,
}

/// An empty color follows the chart default.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceCumulativeDeltaState {
    /// 0 resets at each session open, 1 never resets.
    #[prost(uint32, tag = "1")]
    pub reset: u32,
    #[prost(string, tag = "2")]
    pub line_color: String,
    #[prost(string, tag = "3")]
    pub up_color: String,
    #[prost(string, tag = "4")]
    pub down_color: String,
}

/// An empty color follows the order-flow default.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceDeltaHistogramState {
    #[prost(string, tag = "1")]
    pub up_color: String,
    #[prost(string, tag = "2")]
    pub down_color: String,
}

/// An empty color follows the chart's up (bid) or down (ask) color.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceOrderBookDepthState {
    #[prost(uint32, tag = "1")]
    pub levels_per_side: u32,
    /// Logical-pixel length of the largest level's bar.
    #[prost(uint32, tag = "2")]
    pub max_width: u32,
    #[prost(uint32, tag = "3")]
    pub opacity_percent: u32,
    #[prost(bool, tag = "4")]
    pub hide_sizes: bool,
    #[prost(string, tag = "5")]
    pub bid_color: String,
    #[prost(string, tag = "6")]
    pub ask_color: String,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceBigTradesState {
    /// 0, 1 and 2 select the weak, medium and strong automatic filter; 3 the fixed minimum.
    #[prost(uint32, tag = "1")]
    pub filter: u32,
    /// Exact IEEE-754 bits of the fixed minimum order volume.
    #[prost(fixed64, tag = "2")]
    pub minimum_volume_bits: u64,
    /// 0 small, 1 medium, 2 large bubbles.
    #[prost(uint32, tag = "3")]
    pub size: u32,
    #[prost(bool, tag = "4")]
    pub show_volume: bool,
    #[prost(bool, tag = "5")]
    pub hidden: bool,
}

/// Durable chart presentation preferences that are independent of market data.
///
/// Colors are stored as bounded CSS strings understood by Aeris Charts. Numeric line
/// widths/styles use integers because current product controls expose discrete
/// professional chart choices rather than arbitrary floating-point values.
#[derive(Clone, PartialEq, Eq, prost::Message)]
#[allow(clippy::struct_excessive_bools)] // Protobuf mirrors independent persisted UI toggles.
pub struct WorkspaceChartAppearanceState {
    #[prost(bool, tag = "1")]
    pub grid_visible: bool,
    #[prost(string, tag = "2")]
    pub grid_color: String,
    #[prost(uint32, tag = "3")]
    pub grid_style: u32,
    #[prost(string, tag = "4")]
    pub crosshair_color: String,
    #[prost(uint32, tag = "5")]
    pub crosshair_width: u32,
    #[prost(uint32, tag = "6")]
    pub crosshair_style: u32,
    #[prost(string, tag = "7")]
    pub up_color: String,
    #[prost(string, tag = "8")]
    pub down_color: String,
    #[prost(string, tag = "9")]
    pub wick_up_color: String,
    #[prost(string, tag = "10")]
    pub wick_down_color: String,
    #[prost(string, tag = "11")]
    pub border_up_color: String,
    #[prost(string, tag = "12")]
    pub border_down_color: String,
    #[prost(bool, tag = "13")]
    pub wick_visible: bool,
    #[prost(bool, tag = "14")]
    pub border_visible: bool,
    #[prost(bool, tag = "15")]
    pub open_visible: bool,
    #[prost(bool, tag = "16")]
    pub thin_bars: bool,
    #[prost(string, tag = "17")]
    pub line_color: String,
    #[prost(uint32, tag = "18")]
    pub line_width: u32,
    #[prost(uint32, tag = "19")]
    pub line_style: u32,
    #[prost(string, tag = "20")]
    pub area_top_color: String,
    // Tag 21 held an earlier area bottom color whose saved values predate the engine gradient;
    // it stays unused so those values are never read as the current field.
    #[prost(string, tag = "22")]
    pub baseline_top_color: String,
    #[prost(string, tag = "23")]
    pub baseline_bottom_color: String,
    /// Empty in layouts saved before canvas background control existed; restores as theme-following.
    #[prost(string, tag = "24")]
    pub background_color: String,
    /// Empty in layouts saved before canvas text control existed; restores as theme-following.
    #[prost(string, tag = "25")]
    pub text_color: String,
    /// Empty in layouts saved before line, area and baseline colors carried provenance; those
    /// layouts pinned the engine's reported defaults, which restore as default-following.
    #[prost(string, tag = "26")]
    pub area_bottom_color: String,
    /// Whether `crosshair_style` is the user's choice rather than the chart default. Absent in
    /// layouts saved before it existed, which stored the chart's former dashed default as a
    /// plain number.
    #[prost(bool, optional, tag = "27")]
    pub crosshair_style_custom: Option<bool>,
    /// Empty in layouts saved before axis color control existed; restores as theme-following.
    #[prost(string, tag = "28")]
    pub axis_border_color: String,
    /// Empty in layouts saved before axis color control existed; restores as theme-following.
    #[prost(string, tag = "29")]
    pub axis_text_color: String,
}

/// Durable host-owned state for one native Study Runtime instance.
///
/// `local_id` is a monotonically increasing creation identity stable only
/// inside this chart document. Runtime-assigned `StudyInstanceId` values are
/// deliberately never persisted.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartStudyState {
    #[prost(uint64, tag = "1")]
    pub local_id: u64,
    /// Stable SDK implementation identifier such as `builtin.sma`.
    #[prost(string, tag = "2")]
    pub identifier: String,
    /// Version of the implementation's durable settings/dependency contract.
    #[prost(uint32, tag = "3")]
    pub implementation_revision: u32,
    #[prost(message, repeated, tag = "4")]
    pub settings: Vec<WorkspaceStudySettingState>,
    #[prost(message, repeated, tag = "5")]
    pub dependencies: Vec<WorkspaceStudyDependencyState>,
    #[prost(bool, tag = "6")]
    pub visible: bool,
    /// Stable output interface captured for dependency restoration/migration.
    #[prost(string, repeated, tag = "7")]
    pub output_identifiers: Vec<String>,
    /// Output stroke width in CSS px (1-4). Zero, as in layouts saved before this field,
    /// keeps the chart's default width.
    #[prost(uint32, tag = "8")]
    pub line_width: u32,
    /// Output line colors parallel to `output_identifiers`, as CSS strings. Empty, or an empty
    /// entry, keeps the chart's default color for that output.
    #[prost(string, repeated, tag = "9")]
    pub output_colors: Vec<String>,
}

/// Durable static dependency for one native study.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceStudyDependencyState {
    #[prost(enumeration = "WorkspaceStudyDependencyKind", tag = "1")]
    pub kind: i32,
    /// Required only for market dependencies.
    #[prost(enumeration = "WorkspaceStudyMarketStream", repeated, tag = "2")]
    pub streams: Vec<i32>,
    /// Required only for an explicit market series. Current-chart dependencies
    /// intentionally resolve against the pane's current selection at runtime.
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
    /// Required only for a prior study-output dependency.
    #[prost(uint64, tag = "4")]
    pub study_local_id: u64,
    /// Stable output identifier on `study_local_id`.
    #[prost(string, tag = "5")]
    pub output_identifier: String,
}

/// Durable exact decimal used by a typed native study setting.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceStudyDecimalState {
    #[prost(sint64, tag = "1")]
    pub mantissa: i64,
    #[prost(uint32, tag = "2")]
    pub scale: u32,
}

/// Durable typed setting value for one native study.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceStudySettingState {
    #[prost(string, tag = "1")]
    pub identifier: String,
    #[prost(oneof = "workspace_study_setting_state::Value", tags = "2, 3, 4, 5, 6")]
    pub value: Option<workspace_study_setting_state::Value>,
}

pub mod workspace_study_setting_state {
    /// Exact persisted setting value; the oneof preserves type across upgrades.
    #[derive(Clone, PartialEq, Eq, prost::Oneof)]
    pub enum Value {
        #[prost(bool, tag = "2")]
        Boolean(bool),
        #[prost(sint64, tag = "3")]
        Integer(i64),
        #[prost(message, tag = "4")]
        Decimal(super::WorkspaceStudyDecimalState),
        #[prost(string, tag = "5")]
        Text(String),
        #[prost(string, tag = "6")]
        Choice(String),
    }
}

/// Durable price alert created from a chart crosshair action.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspacePriceAlertState {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(message, optional, tag = "2")]
    pub instrument: Option<InstallProviderInstrument>,
    #[prost(sint64, tag = "3")]
    pub price: i64,
    #[prost(uint32, tag = "4")]
    pub pane_index: u32,
    #[prost(uint32, tag = "5")]
    pub price_scale_side: u32,
    #[prost(enumeration = "PriceAlertCondition", tag = "6")]
    pub condition: i32,
    #[prost(enumeration = "PriceAlertFrequency", tag = "7")]
    pub frequency: i32,
    #[prost(enumeration = "PriceAlertStatus", tag = "8")]
    pub status: i32,
    #[prost(sint64, tag = "9")]
    pub created_at_unix_nanos: i64,
}

/// Durable host-owned state for one native indicator instance.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartIndicatorState {
    #[prost(string, tag = "1")]
    pub kind: String,
    #[prost(bool, tag = "2")]
    pub visible: bool,
    /// Present only for the volume indicator; absent restores its defaults.
    #[prost(message, optional, tag = "3")]
    pub volume: Option<WorkspaceVolumeSettingsState>,
}

/// Durable volume indicator presentation. Empty colors follow the Aeris Charts defaults.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceVolumeSettingsState {
    #[prost(string, tag = "1")]
    pub up_color: String,
    #[prost(string, tag = "2")]
    pub down_color: String,
    #[prost(bool, tag = "3")]
    pub average_enabled: bool,
    /// 0 simple, 1 exponential moving average.
    #[prost(uint32, tag = "4")]
    pub average_kind: u32,
    #[prost(uint32, tag = "5")]
    pub average_length: u32,
    #[prost(string, tag = "6")]
    pub average_color: String,
    /// 0 over the price pane, 1 in a pane of its own.
    #[prost(uint32, tag = "7")]
    pub placement: u32,
}

/// Durable product price-axis customization for one chart pane.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspacePriceAxisState {
    #[prost(uint32, tag = "1")]
    pub flags: u32,
    #[prost(uint32, tag = "2")]
    pub mode: u32,
    #[prost(bool, tag = "3")]
    pub left: bool,
    #[prost(uint32, optional, tag = "4")]
    pub precision: Option<u32>,
}

/// Persisted binary split tree for one workspace tab.
///
/// A leaf has a non-zero `pane_id` and no children. A split has `pane_id == 0`,
/// two children, and stores the first child's share in basis points.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceLayoutState {
    #[prost(uint64, tag = "1")]
    pub pane_id: u64,
    #[prost(enumeration = "WorkspaceSplitAxis", tag = "2")]
    pub split_axis: i32,
    #[prost(uint32, tag = "3")]
    pub ratio_basis_points: u32,
    #[prost(message, optional, boxed, tag = "4")]
    pub first: Option<Box<WorkspaceLayoutState>>,
    #[prost(message, optional, boxed, tag = "5")]
    pub second: Option<Box<WorkspaceLayoutState>>,
}

/// Persisted presentation state for one workspace tab.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceTabState {
    #[prost(uint64, tag = "1")]
    pub workspace_id: u64,
    #[prost(string, tag = "2")]
    pub label: String,
    #[prost(enumeration = "WorkspaceSplitAxis", tag = "3")]
    pub split_axis: i32,
    #[prost(message, repeated, tag = "4")]
    pub panes: Vec<WorkspacePaneState>,
    #[prost(uint64, tag = "5")]
    pub active_pane_id: u64,
    #[prost(uint64, tag = "6")]
    pub generation: u64,
    /// Authoritative nested workspace layout. Legacy flat fields remain for migration.
    #[prost(message, optional, tag = "7")]
    pub layout: Option<WorkspaceLayoutState>,
    /// The saved layout the tab shows. Zero in workspaces saved before the layout library.
    #[prost(uint64, tag = "8")]
    pub saved_layout_id: u64,
}

/// Provider-neutral identity of one demanded bar series.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SeriesKey {
    /// Provider identifier.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Stable canonical instrument identifier.
    #[prost(string, tag = "2")]
    pub instrument_id: String,
    /// Positive value interpreted by `cadence`.
    #[prost(uint32, tag = "3")]
    pub cadence_value: u32,
    /// Canonical series-definition revision.
    #[prost(uint32, tag = "4")]
    pub definition_revision: u32,
    /// Exact entitlement revision used for this series.
    #[prost(string, tag = "5")]
    pub entitlement_id: String,
    /// Cadence kind for the positive `cadence_value`.
    #[prost(enumeration = "SeriesCadence", tag = "6")]
    pub cadence: i32,
}

/// Installs one provider-neutral instrument resolved by an authenticated adapter session.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct InstallProviderInstrument {
    /// Provider identifier.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Provider session generation that resolved the instrument.
    #[prost(uint64, tag = "2")]
    pub session_generation: u64,
    /// Monotonic user selection generation within the provider session.
    #[prost(uint64, tag = "3")]
    pub selection_generation: u64,
    /// Stable canonical instrument identifier.
    #[prost(string, tag = "4")]
    pub instrument_id: String,
    /// Provider routing symbol. This remains outside hot market publications.
    #[prost(string, tag = "5")]
    pub provider_symbol: String,
    /// Human-readable symbol for presentation metadata.
    #[prost(string, tag = "6")]
    pub display_symbol: String,
    /// Provider venue or exchange identity.
    #[prost(string, tag = "7")]
    pub venue_id: String,
    /// Decimal price scale.
    #[prost(uint32, tag = "8")]
    pub price_scale: u32,
    /// Decimal quantity scale.
    #[prost(uint32, tag = "9")]
    pub quantity_scale: u32,
    /// Exact entitlement revision used for subsequent demand.
    #[prost(string, tag = "10")]
    pub entitlement_id: String,
    /// Authoritative minimum price increment in fixed-point `price_scale`
    /// units. Absent means the provider did not expose a safely usable tick;
    /// consumers must not infer one from decimal precision.
    #[prost(int64, optional, tag = "11")]
    pub price_increment: Option<i64>,
    /// Cold provider contract terms kept behind one allocation.
    #[prost(message, optional, boxed, tag = "12")]
    pub contract_metadata: Option<Box<ProviderContractMetadata>>,
}

/// Provider contract terms and practice order constraints. Absent source terms remain unknown.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderContractMetadata {
    /// Point value coefficient in explicit `point_value_scale` currency units.
    #[prost(int64, optional, tag = "1")]
    pub point_value: Option<i64>,
    /// Decimal scale for `point_value`; present exactly when point value is present.
    #[prost(uint32, optional, tag = "2")]
    pub point_value_scale: Option<u32>,
    /// Provider-reported settlement currency.
    #[prost(string, optional, tag = "3")]
    pub currency: Option<String>,
    /// Provider-reported contract expiry as an ISO `YYYY-MM-DD` date when available.
    #[prost(string, optional, tag = "4")]
    pub contract_expiry: Option<String>,
    /// Provider-reported first-notice date as ISO `YYYY-MM-DD` when available.
    #[prost(string, optional, tag = "5")]
    pub first_notice_date: Option<String>,
    /// Provider-reported last-trade date as ISO `YYYY-MM-DD` when available.
    #[prost(string, optional, tag = "6")]
    pub last_trade_date: Option<String>,
    /// Provider-reported weekly exchange sessions. Empty means unavailable, never 24x7.
    #[prost(message, repeated, tag = "7")]
    pub session_hours: Vec<ProviderSessionHours>,
    /// Minimum practice order quantity in fixed-point `InstallProviderInstrument.quantity_scale` units.
    #[prost(int64, optional, tag = "8")]
    pub order_quantity_increment: Option<i64>,
    /// Provider-sourced variable tick rule; absent means fixed increment or unknown.
    #[prost(uint32, optional, tag = "9")]
    pub price_significant_figures: Option<u32>,
    /// Maximum price decimals after applying the exchange's size precision.
    #[prost(uint32, optional, tag = "10")]
    pub price_max_decimals: Option<u32>,
    #[prost(enumeration = "ProviderInstrumentClass", optional, tag = "11")]
    pub instrument_class: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ProviderInstrumentClass {
    Other = 0,
    Equity = 1,
    Option = 2,
    Future = 3,
    ForeignExchange = 4,
    CryptoSpot = 5,
    CryptoPerpetual = 6,
}

/// One provider-reported weekly exchange session segment.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderSessionHours {
    /// ISO weekday where Monday is 1 and Sunday is 7.
    #[prost(uint32, tag = "1")]
    pub weekday: u32,
    #[prost(uint32, tag = "2")]
    pub open_seconds: u32,
    #[prost(uint32, tag = "3")]
    pub close_seconds: u32,
    /// IANA timezone identity supplied by the provider/reference mapping.
    #[prost(string, tag = "4")]
    pub timezone: String,
}
