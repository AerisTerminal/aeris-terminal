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
    /// Fixed chart interval in seconds for the first migration slices.
    #[prost(uint32, tag = "3")]
    pub interval_seconds: u32,
    /// Canonical series-definition revision.
    #[prost(uint32, tag = "4")]
    pub definition_revision: u32,
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
    /// Stable chart or DOM consumer identity.
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

/// Requests complete resident-engine shutdown.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct ShutdownEngine {}

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

/// Requests activation of the existing UI instance.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct ActivateExistingUi {}

/// Orderly connection shutdown notice.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Goodbye {
    /// Human-readable reason.
    #[prost(string, tag = "1")]
    pub reason: String,
}

/// Versioned local engine envelope. Removed tags remain permanently unused.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Envelope {
    /// Protocol version.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Message payload.
    #[prost(
        oneof = "envelope::Payload",
        tags = "2, 3, 4, 5, 6, 7, 15, 16, 17, 18, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38"
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
        /// Existing UI activation. Tag 16.
        #[prost(message, tag = "16")]
        ActivateExistingUi(super::ActivateExistingUi),
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
    }
}
