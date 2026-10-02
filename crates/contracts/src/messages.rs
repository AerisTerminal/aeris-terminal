//! Persisted workspace contracts plus strongly typed in-process market metadata.

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

/// Stable processing stage for actionable, redacted failures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum FailureStage {
    #[default]
    Unspecified = 0,
    ProviderHistory = 1,
    CanonicalValidation = 2,
    MemoryInstall = 3,
    Aggregation = 4,
    Handoff = 9,
    Publication = 10,
    ChartInstall = 12,
    ProviderRealtime = 13,
}

/// Machine-readable runtime fault classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum EngineFaultCode {
    /// Transient failure.
    #[default]
    Retryable = 0,
    /// Engine is offline.
    Offline = 1,
    /// Operation was cancelled.
    Cancelled = 2,
    /// Unrecoverable failure.
    Permanent = 3,
    /// Persisted local state is corrupt.
    CorruptLocalState = 5,
    /// Credentials were rejected.
    Unauthenticated = 7,
}

/// Provider connection state published by the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum ProviderConnectionState {
    /// No active provider connection.
    #[default]
    Disconnected = 0,
    /// A connection or authentication attempt is in progress.
    Connecting = 1,
    /// The provider is available for market work.
    Online = 2,
    /// Continuity is being re-established.
    Recovering = 3,
    /// The provider reached an explicit terminal failure.
    Failed = 4,
}

/// Authoritative readiness state for one consumer's current series demand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum SeriesLoadState {
    /// No usable values are present yet.
    #[default]
    Empty = 0,
    /// Memory or provider work is in progress.
    Resolving = 1,
    /// A usable incomplete range is available.
    Partial = 2,
    /// A covering historical snapshot is available.
    Ready = 3,
    /// Historical state is ready and realtime is advancing.
    Live = 4,
    /// The demand reached an explicit failure.
    Failed = 5,
    /// A newer generation replaced this demand.
    Superseded = 6,
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
}

/// One durable native watchlist row. Live prices remain runtime-owned and are
/// deliberately rebuilt from provider state after restart.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceWatchlistEntryState {
    #[prost(message, optional, tag = "1")]
    pub instrument: Option<InstallProviderInstrument>,
}

/// One locally persisted chart appearance template. Market data, indicators,
/// drawings, and scale state are deliberately excluded.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceChartSettingsTemplateState {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub chart_type: String,
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
    /// Visible native side-panel components: bit 0 order book, bit 1 watchlist.
    #[prost(uint32, tag = "11")]
    pub side_panel_visibility: u32,
    /// Side-panel width in logical pixels.
    #[prost(uint32, tag = "12")]
    pub side_panel_width: u32,
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
    /// Bounded Aeris Charts semantic drawing document. It contains committed
    /// drawings only, never market history, series, indicator definitions, or
    /// renderer/runtime caches.
    #[prost(string, tag = "2")]
    pub chart_state_json: String,
    /// Host-owned native indicators in stable creation order.
    #[prost(message, repeated, tag = "3")]
    pub indicators: Vec<WorkspaceChartIndicatorState>,
    /// Stable product price-axis customization.
    #[prost(message, optional, tag = "4")]
    pub price_axis: Option<WorkspacePriceAxisState>,
    /// Drawing locks are host interaction state keyed by Aeris Charts drawing id.
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
    /// Footprint/CVD/delta and trade-bubble presentation. The retained trade
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
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WorkspaceOrderFlowSettingsState {
    #[prost(uint32, tag = "1")]
    pub display_mode: u32,
    #[prost(bool, tag = "2")]
    pub show_cumulative_delta: bool,
    #[prost(bool, tag = "3")]
    pub show_delta_histogram: bool,
    #[prost(bool, tag = "4")]
    pub show_trade_bubbles: bool,
    /// Exact IEEE-754 bits; zero retains the adaptive per-contract threshold.
    #[prost(fixed64, tag = "5")]
    pub trade_bubble_minimum_volume_bits: u64,
    /// Instrument ticks per footprint row; zero (and absent in older workspaces) is automatic.
    #[prost(uint32, tag = "6")]
    pub ticks_per_row: u32,
    /// Revision 1 records study visibility after CVD and Delta became opt-in indicators.
    /// Older workspaces omit this field and are migrated away from the former default-on panes.
    #[prost(uint32, tag = "7")]
    pub study_visibility_revision: u32,
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
    #[prost(string, tag = "22")]
    pub baseline_top_color: String,
    #[prost(string, tag = "23")]
    pub baseline_bottom_color: String,
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

/// Provider-sourced futures contract terms. Absent fields remain unknown and are never inferred.
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

/// Requests one bounded exact provider-instrument search for a market consumer.
pub const STARTUP_CATALOG_COMMAND_GENERATION: u64 = u32::MAX as u64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchProviderInstruments {
    pub consumer_id: u64,
    pub search_generation: u64,
    pub provider: String,
    pub query: String,
    pub maximum_results: u32,
}

/// Selects one exact instrument from the latest completed provider search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectProviderInstrument {
    pub consumer_id: u64,
    pub selection_generation: u64,
    pub search_generation: u64,
    pub provider: String,
    pub symbol: String,
    pub exchange: String,
    pub entitlement_id: String,
}

/// Bounded provider-neutral presentation metadata for one search result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInstrumentSummary {
    /// Exact provider routing symbol used for selection requests.
    pub symbol: String,
    /// Provider-owned human presentation symbol. Consumers must render this
    /// rather than deriving a label from `symbol`.
    pub display_symbol: String,
    pub exchange: String,
    pub name: Option<String>,
    pub product_code: Option<String>,
    pub instrument_type: Option<String>,
    pub expiration_date: Option<String>,
}

/// Completed bounded provider-instrument search publication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInstrumentSearchResult {
    pub consumer_id: u64,
    pub provider: String,
    pub provider_generation: u64,
    pub search_generation: u64,
    pub instruments: Vec<ProviderInstrumentSummary>,
}

/// Coarse catalog-command failure safe for presentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum ProviderCatalogRejectionReason {
    #[default]
    Unspecified = 0,
    SearchRejected = 1,
    SupersededSearch = 2,
    InstrumentUnavailable = 3,
    SubscriptionRejected = 4,
    DispatchUnavailable = 5,
    SearchTimedOut = 6,
    SelectionTimedOut = 7,
}

/// Generation-fenced provider catalog command rejection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderCatalogRejected {
    pub consumer_id: u64,
    pub provider: String,
    pub provider_generation: Option<u64>,
    pub command_generation: u64,
    pub reason: ProviderCatalogRejectionReason,
}

/// Runtime-owned provider health publication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderState {
    pub provider: String,
    pub state: ProviderConnectionState,
    pub generation: u64,
    pub detail: Option<String>,
    pub transport_rtt_nanos: Option<u64>,
}

/// Redacted runtime fault.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fault {
    pub code: EngineFaultCode,
    pub redacted_detail: String,
}
