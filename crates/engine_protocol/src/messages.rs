//! Prost messages for authenticated engine control, market publication, workspace, and lifecycle IPC.

/// Kind of client connecting to the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ClientKind {
    /// Interactive UI process.
    Ui = 0,
    /// Read-only diagnostics tooling.
    Diagnostics = 1,
}

/// Persisted engine resource mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ResourceMode {
    /// Interactive desktop session.
    Interactive = 0,
    /// Warm background persistence.
    Warm = 1,
    /// Constrained background persistence.
    Constrained = 2,
    /// Offline suspension.
    OfflineSuspended = 3,
    /// Warm background engine with selected market sessions kept live.
    MarketsLive = 4,
}

/// Per-consumer presentation and retention priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ConsumerResourceClass {
    /// Visible pane in the selected workspace.
    Foreground = 0,
    /// Pane in another open workspace; publication may use a lower cadence.
    Background = 1,
    /// Recent or pinned demand retained without continuous UI publication.
    Warm = 2,
    /// The UI no longer owns publication or an upstream subscription.
    Detached = 3,
}

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
    SegmentEncode = 5,
    Encryption = 6,
    FilesystemWrite = 7,
    CatalogCommit = 8,
    Handoff = 9,
    Publication = 10,
    IpcSend = 11,
    ChartInstall = 12,
    ProviderRealtime = 13,
}

/// Durable user-selected resident-engine lifetime policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum EngineLifetimeMode {
    /// Closing the final desktop requests complete engine termination.
    ExitCompletely = 0,
    /// Closing the desktop retains bounded in-memory and local-cache state.
    KeepEngineWarm = 1,
    /// Closing the desktop also retains explicitly permitted hot provider sessions.
    KeepMarketsLive = 2,
}

/// Observable process shutdown state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum EngineShutdownState {
    /// The engine accepts authenticated work.
    Running = 0,
    /// Complete shutdown was accepted and bounded teardown is underway.
    ShuttingDown = 1,
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
    /// Memory, disk, or provider work is in progress.
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

/// Engine-decided mutation carried by one incremental series publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum SeriesUpdateOperation {
    Unspecified = 0,
    ReviseTail = 1,
    AppendTail = 2,
}

/// Persistence state reported independently from in-memory market readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum PersistenceState {
    /// No persistence was needed for this publication.
    NotRequested = 0,
    /// Persistence is pending behind the usable in-memory publication.
    Pending = 1,
    /// The publication is durable.
    Durable = 2,
    /// Persistence failed while in-memory state remains usable.
    Degraded = 3,
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

/// Direction of one physical stream inside a paired client session.
///
/// A session owns two streams so blocking reads never share a transport
/// handle with writes: the command stream is write-only for the client and
/// the event stream is read-only. Both hellos carry the same session nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum StreamRole {
    /// No direction claimed; rejected during session pairing.
    Unspecified = 0,
    /// Client-to-engine commands and session negotiation.
    Command = 1,
    /// Engine-to-client replies, readiness, and pushed market events.
    Event = 2,
}

/// First authenticated client message.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ClientHello {
    /// Protocol version.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Opaque installation credential.
    #[prost(bytes = "vec", tag = "2")]
    pub installation_token: Vec<u8>,
    /// Client kind.
    #[prost(enumeration = "ClientKind", tag = "3")]
    pub client_kind: i32,
    /// Immutable release identity embedded in the client binary.
    #[prost(string, tag = "4")]
    pub release_identity: String,
    /// Monotonic installation generation embedded in the client binary.
    #[prost(uint64, tag = "5")]
    pub install_generation: u64,
    /// Random session nonce pairing this stream with its sibling stream.
    #[prost(uint64, tag = "6")]
    pub session_nonce: u64,
    /// Direction of this physical stream inside the session.
    #[prost(enumeration = "StreamRole", tag = "7")]
    pub stream_role: i32,
}

/// Engine readiness reply.
#[derive(Clone, PartialEq, prost::Message)]
pub struct EngineReady {
    /// Protocol version.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Process epoch.
    #[prost(uint64, tag = "2")]
    pub engine_epoch: u64,
    /// Current workspace revision.
    #[prost(uint64, tag = "3")]
    pub workspace_revision: u64,
    /// Compatible lifecycle contract revision supported by this engine binary.
    #[prost(uint32, tag = "4")]
    pub lifecycle_contract_revision: u32,
    /// Immutable release identity embedded in the engine binary.
    #[prost(string, tag = "5")]
    pub release_identity: String,
    /// Monotonic installation generation embedded in the engine binary.
    #[prost(uint64, tag = "6")]
    pub install_generation: u64,
}

/// Requests persisted workspace restoration.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct RestoreWorkspace {}

/// Bounded recently used series persisted with the workspace.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct HotSeries {
    /// Provider identifier.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Market identifier.
    #[prost(string, tag = "2")]
    pub market: String,
    /// Chart interval seconds.
    #[prost(uint32, tag = "3")]
    pub interval_seconds: u32,
    /// Bounded usage score.
    #[prost(uint32, tag = "4")]
    pub score: u32,
    /// Last use in Unix seconds.
    #[prost(uint64, tag = "5")]
    pub last_used_unix_seconds: u64,
    /// Last provider watermark.
    #[prost(uint64, tag = "6")]
    pub provider_watermark: u64,
    /// Last series watermark.
    #[prost(uint64, tag = "7")]
    pub series_watermark: u64,
    /// Visible range start.
    #[prost(sint64, optional, tag = "8")]
    pub viewport_start_unix_nanos: Option<i64>,
    /// Visible range end.
    #[prost(sint64, optional, tag = "9")]
    pub viewport_end_unix_nanos: Option<i64>,
    /// Provider account identity needed for safe restore.
    #[prost(string, tag = "10")]
    pub account_id: String,
    /// Canonical provider-neutral instrument identity.
    #[prost(string, tag = "11")]
    pub instrument_id: String,
    /// Entitlement identity that fences provider/account reuse.
    #[prost(string, tag = "12")]
    pub entitlement_id: String,
    /// Exact canonical cadence kind.
    #[prost(enumeration = "SeriesCadence", tag = "13")]
    pub cadence: i32,
    /// Exact cadence value.
    #[prost(uint32, tag = "14")]
    pub cadence_value: u32,
    /// Bar-definition revision.
    #[prost(uint32, tag = "15")]
    pub definition_revision: u32,
    /// Explicit user pin retained ahead of recency-only entries.
    #[prost(bool, tag = "16")]
    pub pinned: bool,
    /// Bounded workspace identities that recently used this series.
    #[prost(uint64, repeated, tag = "17")]
    pub workspace_ids: Vec<u64>,
    /// Durable local coverage start.
    #[prost(sint64, optional, tag = "18")]
    pub coverage_start_unix_nanos: Option<i64>,
    /// Durable local coverage end.
    #[prost(sint64, optional, tag = "19")]
    pub coverage_end_unix_nanos: Option<i64>,
    /// Provider-native symbol required for warm provider selection.
    #[prost(string, tag = "20")]
    pub provider_symbol: String,
    /// Provider venue/exchange identity.
    #[prost(string, tag = "21")]
    pub venue_id: String,
    /// Stable presentation label.
    #[prost(string, tag = "22")]
    pub display_symbol: String,
    /// Fixed-point price scale.
    #[prost(uint32, tag = "23")]
    pub price_scale: u32,
    /// Fixed-point quantity scale.
    #[prost(uint32, tag = "24")]
    pub quantity_scale: u32,
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
    /// Whether warm persistence is enabled.
    #[prost(bool, tag = "6")]
    pub warm_mode_enabled: bool,
    /// Resource mode.
    #[prost(enumeration = "ResourceMode", tag = "7")]
    pub resource_mode: i32,
    /// Workspace schema revision.
    #[prost(uint32, tag = "8")]
    pub schema_revision: u32,
    /// Hot-set schema revision.
    #[prost(uint32, tag = "9")]
    pub cache_manifest_revision: u32,
    /// Bounded recent-series set.
    #[prost(message, repeated, tag = "10")]
    pub hot_series: Vec<HotSeries>,
    /// Durable final-desktop lifetime behavior.
    #[prost(enumeration = "EngineLifetimeMode", tag = "11")]
    pub lifetime_mode: i32,
    /// Whether the engine should start with the user's OS session.
    #[prost(bool, tag = "12")]
    pub autostart_enabled: bool,
    /// Explicit user permission to retain provider sessions without a desktop.
    #[prost(bool, tag = "13")]
    pub markets_live_permitted: bool,
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

/// Revision- and generation-fenced workspace composition replacement.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SetWorkspaceLayout {
    #[prost(uint64, tag = "1")]
    pub workspace_revision: u64,
    #[prost(uint64, tag = "2")]
    pub layout_generation: u64,
    #[prost(uint64, tag = "3")]
    pub active_workspace_id: u64,
    #[prost(message, repeated, tag = "4")]
    pub workspace_tabs: Vec<WorkspaceTabState>,
}

/// Revision-fenced selection persistence request.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetSelection {
    /// Market.
    #[prost(string, tag = "1")]
    pub market: String,
    /// Interval seconds.
    #[prost(uint32, tag = "2")]
    pub interval_seconds: u32,
    /// Based-on workspace revision.
    #[prost(uint64, tag = "3")]
    pub workspace_revision: u64,
    /// Client selection generation.
    #[prost(uint64, tag = "4")]
    pub selection_generation: u64,
    /// Provider; empty preserves the current provider.
    #[prost(string, tag = "5")]
    pub provider: String,
}

/// Revision-fenced watchlist replacement.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetWatchlist {
    /// Complete watched-market replacement.
    #[prost(string, repeated, tag = "1")]
    pub markets: Vec<String>,
    /// Based-on workspace revision.
    #[prost(uint64, tag = "2")]
    pub workspace_revision: u64,
}

/// Stable viewport persistence request.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct SetViewport {
    /// Visible start.
    #[prost(sint64, tag = "1")]
    pub start_unix_nanos: i64,
    /// Visible end.
    #[prost(sint64, tag = "2")]
    pub end_unix_nanos: i64,
    /// Active selection generation.
    #[prost(uint64, tag = "3")]
    pub selection_generation: u64,
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

/// Authenticated client attachment to engine-owned market state.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct AttachClient {
    /// Stable client identity for this desktop lifetime.
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
}

/// Releases all market demand owned by one client.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct DetachClient {
    /// Stable client identity being detached.
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
}

/// Registers one independently generated UI consumer.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct RegisterConsumer {
    /// Owning client.
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
    /// Owning workspace tab.
    #[prost(uint64, tag = "2")]
    pub workspace_id: u64,
    /// Stable chart or Order Book consumer identity.
    #[prost(uint64, tag = "3")]
    pub consumer_id: u64,
}

/// Replaces one consumer's authoritative series demand immediately.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SeriesDemand {
    /// Consumer being updated.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Per-consumer generation; later values supersede earlier work.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    /// Requested canonical series.
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
}

/// Updates the visible history range for one current demand.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct ViewportDemand {
    /// Consumer being updated.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Demand generation this viewport belongs to.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    /// Visible range start.
    #[prost(sint64, tag = "3")]
    pub start_unix_nanos: i64,
    /// Visible range end.
    #[prost(sint64, tag = "4")]
    pub end_unix_nanos: i64,
}

/// Updates presentation priority without changing market identity.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct VisibilityDemand {
    /// Consumer being updated.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Whether this consumer is currently visible.
    #[prost(bool, tag = "2")]
    pub visible: bool,
    /// Exact foreground/background/warm/detached resource class.
    #[prost(enumeration = "ConsumerResourceClass", tag = "3")]
    pub resource_class: i32,
}

/// Removes one consumer without affecting consumers sharing its upstream state.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct RemoveConsumer {
    /// Consumer being removed.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
}

/// Changes engine resource policy without changing user market demand.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct SetEngineResourceMode {
    /// Requested resource mode.
    #[prost(enumeration = "ResourceMode", tag = "1")]
    pub resource_mode: i32,
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

/// Confirms that the engine accepted one generation-fenced instrument install.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderInstrumentInstalled {
    /// Provider identifier.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Accepted provider session generation.
    #[prost(uint64, tag = "2")]
    pub session_generation: u64,
    /// Accepted selection generation.
    #[prost(uint64, tag = "3")]
    pub selection_generation: u64,
    /// Accepted canonical instrument identifier.
    #[prost(string, tag = "4")]
    pub instrument_id: String,
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

/// Completed provider-instrument selection with canonical engine identity.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProviderInstrumentSelection {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(message, optional, tag = "2")]
    pub instrument: Option<InstallProviderInstrument>,
    /// Selection-command generation echoed for consumer-side correlation.
    /// This is distinct from the installed instrument's provider-wide
    /// `selection_generation`, which fences provider/session state.
    #[prost(uint64, tag = "3")]
    pub command_generation: u64,
}

/// Requests complete resident-engine shutdown.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct ShutdownEngine {}

/// Revision-fenced durable engine-lifecycle preference mutation.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct SetEngineLifecycle {
    /// Expected workspace revision.
    #[prost(uint64, tag = "1")]
    pub workspace_revision: u64,
    /// Durable lifetime mode.
    #[prost(enumeration = "EngineLifetimeMode", tag = "2")]
    pub lifetime_mode: i32,
    /// Whether per-user OS-session autostart is enabled.
    #[prost(bool, tag = "3")]
    pub autostart_enabled: bool,
    /// Explicit permission for background provider sessions.
    #[prost(bool, tag = "4")]
    pub markets_live_permitted: bool,
}

/// Requests one bounded engine lifecycle/resource status snapshot.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct GetEngineStatus {}

/// Bounded resident-engine process and resource diagnostics.
#[derive(Clone, PartialEq, prost::Message)]
pub struct EngineStatus {
    /// Operating-system process identifier.
    #[prost(uint32, tag = "1")]
    pub process_id: u32,
    /// Durable user-selected lifetime policy.
    #[prost(enumeration = "EngineLifetimeMode", tag = "2")]
    pub lifetime_mode: i32,
    /// Current operational resource mode.
    #[prost(enumeration = "ResourceMode", tag = "3")]
    pub resource_mode: i32,
    /// Number of attached desktop client identities.
    #[prost(uint32, tag = "4")]
    pub connected_desktop_clients: u32,
    /// Current provider states, bounded by registered providers.
    #[prost(message, repeated, tag = "5")]
    pub providers: Vec<ProviderState>,
    /// Number of canonical series retained in memory.
    #[prost(uint32, tag = "6")]
    pub retained_series: u32,
    /// Number of canonical bars retained in memory.
    #[prost(uint64, tag = "7")]
    pub retained_bars: u64,
    /// Approximate canonical-series heap bytes.
    #[prost(uint64, tag = "8")]
    pub approximate_series_bytes: u64,
    /// Current process shutdown state.
    #[prost(enumeration = "EngineShutdownState", tag = "9")]
    pub shutdown_state: i32,
    /// Whether per-user OS-session autostart is enabled.
    #[prost(bool, tag = "10")]
    pub autostart_enabled: bool,
    /// Whether unattended provider retention is explicitly permitted.
    #[prost(bool, tag = "11")]
    pub markets_live_permitted: bool,
}

/// Authoritative state of one consumer generation.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SeriesState {
    /// Target consumer.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Target generation.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    /// Target series.
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
    /// Market readiness.
    #[prost(enumeration = "SeriesLoadState", tag = "4")]
    pub state: i32,
    /// Independent persistence readiness.
    #[prost(enumeration = "PersistenceState", tag = "5")]
    pub persistence: i32,
    /// Optional redacted status detail.
    #[prost(string, optional, tag = "6")]
    pub detail: Option<String>,
}

/// One exact fixed-point canonical market bar on the IPC boundary.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct MarketBar {
    /// Provider sequence after canonical continuity validation.
    #[prost(uint64, tag = "1")]
    pub source_sequence: u64,
    /// Exchange bar timestamp in Unix seconds.
    #[prost(sint64, tag = "2")]
    pub exchange_timestamp_seconds: i64,
    /// Fixed-point open.
    #[prost(sint64, tag = "3")]
    pub open: i64,
    /// Fixed-point high.
    #[prost(sint64, tag = "4")]
    pub high: i64,
    /// Fixed-point low.
    #[prost(sint64, tag = "5")]
    pub low: i64,
    /// Fixed-point close.
    #[prost(sint64, tag = "6")]
    pub close: i64,
    /// Fixed-point volume.
    #[prost(sint64, tag = "7")]
    pub volume: i64,
    /// Exact exchange ordering timestamp in Unix nanoseconds.
    #[prost(sint64, tag = "8")]
    pub exchange_timestamp_unix_nanos: i64,
}

/// Immutable covering bar snapshot for one consumer generation.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SeriesSnapshot {
    /// Target consumer.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Target demand generation.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    /// Published canonical series.
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
    /// Provider session generation.
    #[prost(uint64, tag = "4")]
    pub provider_generation: u64,
    /// Decimal scale for OHLC fixed-point values.
    #[prost(uint32, tag = "5")]
    pub price_scale: u32,
    /// Decimal scale for volume fixed-point values.
    #[prost(uint32, tag = "6")]
    pub quantity_scale: u32,
    /// Ordered covering values.
    #[prost(message, repeated, tag = "7")]
    pub bars: Vec<MarketBar>,
    /// Monotonic publication generation within the provider session.
    #[prost(uint64, tag = "8")]
    pub publication_generation: u64,
    /// Whether the newest bar is still forming and may be revised.
    #[prost(bool, tag = "9")]
    pub forming: bool,
}

/// Incremental bar publication after a covering snapshot.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct SeriesUpdate {
    /// Target consumer.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Target demand generation.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    /// Published canonical series.
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
    /// Provider session generation.
    #[prost(uint64, tag = "4")]
    pub provider_generation: u64,
    /// Forming or completed bar.
    #[prost(message, optional, tag = "5")]
    pub bar: Option<MarketBar>,
    /// True while the current interval is still forming.
    #[prost(bool, tag = "6")]
    pub forming: bool,
    /// Monotonic publication generation within the provider session.
    #[prost(uint64, tag = "7")]
    pub publication_generation: u64,
    /// Exact mutation already decided by the canonical engine owner.
    #[prost(enumeration = "SeriesUpdateOperation", tag = "8")]
    pub operation: i32,
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
}

/// Recoverability state of one engine-owned order book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum OrderBookState {
    Unspecified = 0,
    AwaitingSnapshot = 1,
    Ready = 2,
    Stale = 3,
    SequenceGap = 4,
    CrossedBook = 5,
    InvalidUpdate = 6,
}

/// One fixed-point level in a bounded engine-owned order-book image.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct OrderBookLevel {
    #[prost(sint64, tag = "1")]
    pub price: i64,
    #[prost(sint64, tag = "2")]
    pub quantity: i64,
    #[prost(uint32, optional, tag = "3")]
    pub order_count: Option<u32>,
    /// Traded quantity accumulated at this price during the resident book session.
    #[prost(sint64, tag = "4")]
    pub traded_volume: i64,
}

/// Latest conflated top-N image of one authoritative engine-owned order book.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct OrderBookSnapshot {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    #[prost(string, tag = "3")]
    pub provider: String,
    #[prost(string, tag = "4")]
    pub instrument_id: String,
    #[prost(string, tag = "5")]
    pub entitlement_id: String,
    #[prost(uint64, tag = "6")]
    pub provider_generation: u64,
    #[prost(uint64, tag = "7")]
    pub selection_generation: u64,
    #[prost(uint64, tag = "8")]
    pub revision: u64,
    #[prost(uint64, tag = "9")]
    pub source_watermark: u64,
    #[prost(enumeration = "OrderBookState", tag = "10")]
    pub state: i32,
    #[prost(message, repeated, tag = "11")]
    pub bids: Vec<OrderBookLevel>,
    #[prost(message, repeated, tag = "12")]
    pub asks: Vec<OrderBookLevel>,
    /// Independently observed best bid from the provider BBO stream.
    #[prost(message, optional, tag = "13")]
    pub best_bid: Option<OrderBookLevel>,
    /// Independently observed best ask from the provider BBO stream.
    #[prost(message, optional, tag = "14")]
    pub best_ask: Option<OrderBookLevel>,
    /// Source sequence of the latest BBO observation.
    #[prost(uint64, tag = "15")]
    pub bbo_source_watermark: u64,
}

/// Stage-specific terminal or degraded result for one demand.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct DemandError {
    /// Target consumer.
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    /// Target generation.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    /// Machine-readable fault class.
    #[prost(enumeration = "EngineFaultCode", tag = "3")]
    pub code: i32,
    /// Owning processing stage, such as `provider_history` or `memory_install`.
    #[prost(string, tag = "4")]
    pub stage: String,
    /// Redacted error detail.
    #[prost(string, tag = "5")]
    pub detail: String,
    /// Canonical series identity when the failure belongs to market demand.
    #[prost(message, optional, tag = "6")]
    pub series: Option<SeriesKey>,
    /// Stable machine-readable stage.
    #[prost(enumeration = "FailureStage", tag = "7")]
    pub stage_code: i32,
    /// Redacted causal context safe for diagnostics and presentation.
    #[prost(string, tag = "8")]
    pub cause: String,
    /// Elapsed time in milliseconds when the stage is timed.
    #[prost(uint64, optional, tag = "9")]
    pub elapsed_millis: Option<u64>,
}

/// Aggressor side carried by provider-neutral order-flow publications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum OrderFlowAggressor {
    Unknown = 0,
    Buy = 1,
    Sell = 2,
}

/// View-ready fixed-point footprint/profile level.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct OrderFlowLevel {
    #[prost(sint64, tag = "1")]
    pub price: i64,
    #[prost(sint64, tag = "2")]
    pub bid_volume: i64,
    #[prost(sint64, tag = "3")]
    pub ask_volume: i64,
    #[prost(uint64, tag = "4")]
    pub trade_count: u64,
    /// Bounded time-at-price observations for future market-profile rendering.
    #[prost(uint64, tag = "5")]
    pub time_at_price_count: u64,
}

/// One bounded view-ready tape item.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct OrderFlowTrade {
    #[prost(uint64, tag = "1")]
    pub source_sequence: u64,
    #[prost(sint64, tag = "2")]
    pub exchange_timestamp_unix_nanos: i64,
    #[prost(sint64, tag = "3")]
    pub price: i64,
    #[prost(sint64, tag = "4")]
    pub quantity: i64,
    #[prost(enumeration = "OrderFlowAggressor", tag = "5")]
    pub aggressor: i32,
}

/// Covering engine-owned footprint, delta, volume-profile, market-profile, and tape image.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct OrderFlowSnapshot {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
    #[prost(uint64, tag = "4")]
    pub provider_generation: u64,
    #[prost(uint64, tag = "5")]
    pub publication_generation: u64,
    #[prost(uint64, tag = "6")]
    pub source_watermark: u64,
    #[prost(sint64, tag = "7")]
    pub cumulative_delta: i64,
    #[prost(message, repeated, tag = "8")]
    pub levels: Vec<OrderFlowLevel>,
    #[prost(message, repeated, tag = "9")]
    pub tape: Vec<OrderFlowTrade>,
}

/// Incremental engine-owned order-flow update.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct OrderFlowUpdate {
    #[prost(uint64, tag = "1")]
    pub consumer_id: u64,
    #[prost(uint64, tag = "2")]
    pub generation: u64,
    #[prost(message, optional, tag = "3")]
    pub series: Option<SeriesKey>,
    #[prost(uint64, tag = "4")]
    pub provider_generation: u64,
    #[prost(uint64, tag = "5")]
    pub publication_generation: u64,
    #[prost(sint64, tag = "6")]
    pub cumulative_delta: i64,
    #[prost(message, optional, tag = "7")]
    pub level: Option<OrderFlowLevel>,
    #[prost(message, optional, tag = "8")]
    pub trade: Option<OrderFlowTrade>,
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

/// Orderly connection shutdown notice.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Goodbye {
    /// Human-readable reason.
    #[prost(string, tag = "1")]
    pub reason: String,
}

/// Versioned local engine envelope. Removed tags remain permanently unused:
/// tags 8-14, 19-24, and 39-40 predate the paired-session contract, and tag
/// 16 carried the removed single-instance UI activation command.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Envelope {
    /// Protocol version.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Target market consumer for pushed publications; zero for commands and replies.
    #[prost(uint64, tag = "8")]
    pub target_consumer_id: u64,
    /// Message payload.
    #[prost(
        oneof = "envelope::Payload",
        tags = "2, 3, 4, 5, 6, 7, 15, 17, 18, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61"
    )]
    pub payload: Option<envelope::Payload>,
}

/// Envelope payloads.
pub mod envelope {
    /// Versioned local persistence and lifecycle message.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        /// Client handshake. Tag 2.
        #[prost(message, tag = "2")]
        ClientHello(super::ClientHello),
        /// Engine readiness. Tag 3.
        #[prost(message, tag = "3")]
        EngineReady(super::EngineReady),
        /// Workspace restore. Tag 4.
        #[prost(message, tag = "4")]
        RestoreWorkspace(super::RestoreWorkspace),
        /// Workspace state. Tag 5.
        #[prost(message, tag = "5")]
        WorkspaceState(super::WorkspaceState),
        /// Selection mutation. Tag 6.
        #[prost(message, tag = "6")]
        SetSelection(super::SetSelection),
        /// Watchlist mutation. Tag 7.
        #[prost(message, tag = "7")]
        SetWatchlist(super::SetWatchlist),
        /// Fault. Tag 15.
        #[prost(message, tag = "15")]
        Fault(super::Fault),
        /// Orderly shutdown. Tag 17.
        #[prost(message, tag = "17")]
        Goodbye(super::Goodbye),
        /// Viewport mutation. Tag 18.
        #[prost(message, tag = "18")]
        SetViewport(super::SetViewport),
        /// Client market attachment. Tag 25.
        #[prost(message, tag = "25")]
        AttachClient(super::AttachClient),
        /// Client market detachment. Tag 26.
        #[prost(message, tag = "26")]
        DetachClient(super::DetachClient),
        /// Consumer registration. Tag 27.
        #[prost(message, tag = "27")]
        RegisterConsumer(super::RegisterConsumer),
        /// Series demand replacement. Tag 28.
        #[prost(message, tag = "28")]
        SeriesDemand(super::SeriesDemand),
        /// Viewport demand. Tag 29.
        #[prost(message, tag = "29")]
        ViewportDemand(super::ViewportDemand),
        /// Visibility demand. Tag 30.
        #[prost(message, tag = "30")]
        VisibilityDemand(super::VisibilityDemand),
        /// Consumer removal. Tag 31.
        #[prost(message, tag = "31")]
        RemoveConsumer(super::RemoveConsumer),
        /// Resource-mode command. Tag 32.
        #[prost(message, tag = "32")]
        SetEngineResourceMode(super::SetEngineResourceMode),
        /// Complete engine shutdown command. Tag 33.
        #[prost(message, tag = "33")]
        ShutdownEngine(super::ShutdownEngine),
        /// Series readiness state. Tag 34.
        #[prost(message, tag = "34")]
        SeriesState(super::SeriesState),
        /// Covering series snapshot. Tag 35.
        #[prost(message, tag = "35")]
        SeriesSnapshot(super::SeriesSnapshot),
        /// Incremental series update. Tag 36.
        #[prost(message, tag = "36")]
        SeriesUpdate(super::SeriesUpdate),
        /// Provider health state. Tag 37.
        #[prost(message, tag = "37")]
        ProviderState(super::ProviderState),
        /// Demand-specific error. Tag 38.
        #[prost(message, tag = "38")]
        DemandError(super::DemandError),
        /// Provider-neutral instrument install. Tag 41.
        #[prost(message, tag = "41")]
        InstallProviderInstrument(super::InstallProviderInstrument),
        /// Accepted provider-neutral instrument install. Tag 42.
        #[prost(message, tag = "42")]
        ProviderInstrumentInstalled(super::ProviderInstrumentInstalled),
        /// Conflated engine-owned order-book image. Tag 43.
        #[prost(message, tag = "43")]
        OrderBookSnapshot(super::OrderBookSnapshot),
        /// Bounded provider-instrument search command. Tag 44.
        #[prost(message, tag = "44")]
        SearchProviderInstruments(super::SearchProviderInstruments),
        /// Exact provider-instrument selection command. Tag 45.
        #[prost(message, tag = "45")]
        SelectProviderInstrument(super::SelectProviderInstrument),
        /// Completed provider-instrument search. Tag 46.
        #[prost(message, tag = "46")]
        ProviderInstrumentSearchResult(super::ProviderInstrumentSearchResult),
        /// Coarse provider catalog command rejection. Tag 47.
        #[prost(message, tag = "47")]
        ProviderCatalogRejected(super::ProviderCatalogRejected),
        /// Completed provider-instrument selection. Tag 48.
        #[prost(message, tag = "48")]
        ProviderInstrumentSelection(super::ProviderInstrumentSelection),
        /// Durable lifecycle preference mutation. Tag 49.
        #[prost(message, tag = "49")]
        SetEngineLifecycle(super::SetEngineLifecycle),
        /// Engine status request. Tag 50.
        #[prost(message, tag = "50")]
        GetEngineStatus(super::GetEngineStatus),
        /// Engine status snapshot. Tag 51.
        #[prost(message, tag = "51")]
        EngineStatus(super::EngineStatus),
        /// Persisted workspace composition replacement. Tag 52.
        #[prost(message, tag = "52")]
        SetWorkspaceLayout(super::SetWorkspaceLayout),
        /// Covering order-flow image. Tag 53.
        #[prost(message, tag = "53")]
        OrderFlowSnapshot(super::OrderFlowSnapshot),
        /// Incremental order-flow update. Tag 54.
        #[prost(message, tag = "54")]
        OrderFlowUpdate(super::OrderFlowUpdate),
        /// Native login start. Tag 55.
        #[prost(message, tag = "55")]
        BeginLogin(super::super::account::BeginLogin),
        /// Pending login cancellation. Tag 56.
        #[prost(message, tag = "56")]
        CancelLogin(super::super::account::CancelLogin),
        /// Sanitized account status request. Tag 57.
        #[prost(message, tag = "57")]
        GetAccountStatus(super::super::account::GetAccountStatus),
        /// Browser authorization address. Tag 58.
        #[prost(message, tag = "58")]
        LoginAuthorization(super::super::account::LoginAuthorization),
        /// Sanitized account view. Tag 59.
        #[prost(message, tag = "59")]
        AccountView(super::super::account::AccountView),
        /// Account sign-out command. Tag 60.
        #[prost(message, tag = "60")]
        SignOut(super::super::account::SignOut),
        /// Engine-owned background account-profile refresh request. Tag 61.
        #[prost(message, tag = "61")]
        RefreshAccountProfile(super::super::account::RefreshAccountProfile),
    }
}
