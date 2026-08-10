//! Prost messages for authenticated workspace persistence and lifecycle IPC.

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
    #[prost(oneof = "envelope::Payload", tags = "2, 3, 4, 5, 6, 7, 15, 16, 17, 18")]
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
    }
}
