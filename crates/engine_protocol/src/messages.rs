//! Shared prost DTOs for workspace persistence and in-process market metadata.

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum FailureStage {
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

/// Machine-readable local engine fault classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum EngineFaultCode {
    /// Transient failure.
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
    /// Peer protocol version is incompatible.
    VersionMismatch = 9,
    /// Peer could not keep up.
    Backpressure = 11,
    /// Message was invalid for the current state.
    MalformedMessage = 13,
    /// Frame exceeded the bound.
    OversizedFrame = 15,
}

/// Provider connection state published by the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ProviderConnectionState {
    /// No active provider connection.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum SeriesLoadState {
    /// No usable values are present yet.
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
    // resident-process lifetime, resource-mode, and local hot-history cache.
    /// Monotonic generation of user-authored workspace composition.
    #[prost(uint64, tag = "14")]
    pub layout_generation: u64,
    /// Selected workspace identity.
    #[prost(uint64, tag = "15")]
    pub active_workspace_id: u64,
    /// Bounded deterministic workspace/tab/pane composition.
    #[prost(message, repeated, tag = "16")]
    pub workspace_tabs: Vec<WorkspaceTabState>,
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
}

/// Requests one bounded exact provider-instrument search for a market consumer.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SearchProviderInstruments {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(uint64, tag = "2")]
    pub search_generation: u64,
    #[prost(string, tag = "3")]
    pub provider: String,
    #[prost(string, tag = "4")]
    pub query: String,
    #[prost(uint32, tag = "5")]
    pub maximum_results: u32,
}

/// Selects one exact instrument from the latest completed provider search.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SelectProviderInstrument {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(uint64, tag = "2")]
    pub selection_generation: u64,
    #[prost(uint64, tag = "3")]
    pub search_generation: u64,
    #[prost(string, tag = "4")]
    pub provider: String,
    #[prost(string, tag = "5")]
    pub symbol: String,
    #[prost(string, tag = "6")]
    pub exchange: String,
    #[prost(string, tag = "7")]
    pub entitlement_id: String,
}

/// Bounded provider-neutral presentation metadata for one search result.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderInstrumentSummary {
    #[prost(string, tag = "1")]
    pub symbol: String,
    #[prost(string, tag = "2")]
    pub exchange: String,
    #[prost(string, optional, tag = "3")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub product_code: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub instrument_type: Option<String>,
    #[prost(string, optional, tag = "6")]
    pub expiration_date: Option<String>,
}

/// Completed bounded provider-instrument search publication.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderInstrumentSearchResult {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(string, tag = "2")]
    pub provider: String,
    #[prost(uint64, tag = "3")]
    pub provider_generation: u64,
    #[prost(uint64, tag = "4")]
    pub search_generation: u64,
    #[prost(message, repeated, tag = "5")]
    pub instruments: Vec<ProviderInstrumentSummary>,
}

/// Coarse catalog-command failure safe to expose across local IPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ProviderCatalogRejectionReason {
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
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderCatalogRejected {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(string, tag = "2")]
    pub provider: String,
    #[prost(uint64, optional, tag = "3")]
    pub provider_generation: Option<u64>,
    #[prost(uint64, tag = "4")]
    pub command_generation: u64,
    #[prost(enumeration = "ProviderCatalogRejectionReason", tag = "5")]
    pub reason: i32,
}

/// Engine-owned provider health publication.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderState {
    /// Provider identifier.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Connection state.
    #[prost(enumeration = "ProviderConnectionState", tag = "2")]
    pub state: i32,
    /// Provider session generation, or zero before the first session.
    #[prost(uint64, tag = "3")]
    pub generation: u64,
    /// Optional redacted state detail.
    #[prost(string, optional, tag = "4")]
    pub detail: Option<String>,
    /// Most recent locally measured provider heartbeat/application-ping RTT.
    /// Absent until a generation has produced a qualified round trip.
    #[prost(uint64, optional, tag = "5")]
    pub transport_rtt_nanos: Option<u64>,
}

/// Redacted engine fault.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Fault {
    /// Fault code.
    #[prost(enumeration = "EngineFaultCode", tag = "1")]
    pub code: i32,
    /// Redacted human-readable detail.
    #[prost(string, tag = "2")]
    pub redacted_detail: String,
}

/// Versioned local workspace-persistence envelope.
///
/// Every former engine-control/publication tag remains permanently retired;
/// only tag 5 (`WorkspaceState`) is written by current builds.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Envelope {
    /// Persistence protocol version.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Workspace payload. Historical non-workspace envelope tags are ignored.
    #[prost(oneof = "envelope::Payload", tags = "5")]
    pub payload: Option<envelope::Payload>,
}

/// Envelope payloads.
pub mod envelope {
    /// Versioned local persistence payload.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        /// Workspace state. Tag 5.
        #[prost(message, tag = "5")]
        WorkspaceState(super::WorkspaceState),
    }
}
