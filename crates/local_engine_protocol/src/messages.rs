//! Prost wire messages. Field numbers are part of the stable wire contract:
//! never reuse, renumber, or remove a tag; only append new fields or payloads.

/// Kind of client connecting to the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ClientKind {
    /// Interactive UI process. Tag 0.
    Ui = 0,
    /// Read-only diagnostics tooling. Tag 1.
    Diagnostics = 1,
}

/// Engine resource mode advertised in the workspace state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ResourceMode {
    /// Full-fidelity interactive streaming. Tag 0.
    Interactive = 0,
    /// Reduced-rate warm mode. Tag 1.
    Warm = 1,
    /// Severely limited resource mode. Tag 2.
    Constrained = 2,
    /// Engine is offline and suspended. Tag 3.
    OfflineSuspended = 3,
}

/// View a client can subscribe to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ViewKind {
    /// Chart view. Tag 0.
    Chart = 0,
    /// Depth-of-market view. Tag 1.
    Dom = 1,
    /// Diagnostics view. Tag 2.
    Diagnostics = 2,
    /// Instrument catalog view. Tag 3.
    Catalog = 3,
}

/// Provider connection state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ProviderConnectionState {
    /// No provider connection. Tag 0.
    Disconnected = 0,
    /// Connection attempt in flight. Tag 1.
    Connecting = 1,
    /// Provider stream is live. Tag 2.
    Connected = 2,
    /// Provider rejected the session. Tag 3.
    Rejected = 3,
    /// Engine is shutting the connection down. Tag 4.
    ShuttingDown = 4,
}

/// Machine-readable fault classification shared by protocol errors and faults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum EngineFaultCode {
    /// Transient failure; the caller may retry. Tag 0.
    Retryable = 0,
    /// Engine is offline. Tag 1.
    Offline = 1,
    /// Operation was cancelled. Tag 2.
    Cancelled = 2,
    /// Unrecoverable failure. Tag 3.
    Permanent = 3,
    /// Persisted local state is corrupt. Tag 4.
    CorruptLocalState = 5,
    /// Credentials were rejected. Tag 6.
    Unauthenticated = 7,
    /// Peer speaks an incompatible protocol version. Tag 8.
    VersionMismatch = 9,
    /// Peer could not keep up with the send rate. Tag 10.
    Backpressure = 11,
    /// Frame decoded but the message was malformed. Tag 12.
    MalformedMessage = 13,
    /// Frame exceeded the negotiated size bound. Tag 14.
    OversizedFrame = 15,
}

/// First message from a client; carries the installation credential.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ClientHello {
    /// Protocol version the client speaks. Tag 1.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Opaque installation token. Tag 2.
    #[prost(bytes = "vec", tag = "2")]
    pub installation_token: Vec<u8>,
    /// Client kind as a [`ClientKind`] value. Tag 3.
    #[prost(enumeration = "ClientKind", tag = "3")]
    pub client_kind: i32,
}

/// Engine handshake reply once it is ready to serve the client.
#[derive(Clone, PartialEq, prost::Message)]
pub struct EngineReady {
    /// Protocol version the engine speaks. Tag 1.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Engine process epoch; changes on every engine restart. Tag 2.
    #[prost(uint64, tag = "2")]
    pub engine_epoch: u64,
    /// Current workspace revision. Tag 3.
    #[prost(uint64, tag = "3")]
    pub workspace_revision: u64,
}

/// Client request for the persisted workspace. Tag-less empty message.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct RestoreWorkspace {}

/// Engine reply describing the persisted workspace.
#[derive(Clone, PartialEq, prost::Message)]
pub struct WorkspaceState {
    /// Active provider identifier. Tag 1.
    #[prost(string, tag = "1")]
    pub provider: String,
    /// Selected market. Tag 2.
    #[prost(string, tag = "2")]
    pub market: String,
    /// Selected chart interval in seconds. Tag 3.
    #[prost(uint32, tag = "3")]
    pub interval_seconds: u32,
    /// Watched markets. Tag 4.
    #[prost(string, repeated, tag = "4")]
    pub watchlist: Vec<String>,
    /// Workspace revision this state belongs to. Tag 5.
    #[prost(uint64, tag = "5")]
    pub workspace_revision: u64,
    /// Whether warm mode is enabled. Tag 6.
    #[prost(bool, tag = "6")]
    pub warm_mode_enabled: bool,
    /// Resource mode as a [`ResourceMode`] value. Tag 7.
    #[prost(enumeration = "ResourceMode", tag = "7")]
    pub resource_mode: i32,
}

/// Client selection change; rejected when the revision or generation is stale.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetSelection {
    /// Market to select. Tag 1.
    #[prost(string, tag = "1")]
    pub market: String,
    /// Chart interval in seconds. Tag 2.
    #[prost(uint32, tag = "2")]
    pub interval_seconds: u32,
    /// Workspace revision the client based this change on. Tag 3.
    #[prost(uint64, tag = "3")]
    pub workspace_revision: u64,
    /// Monotonic client selection generation. Tag 4.
    #[prost(uint64, tag = "4")]
    pub selection_generation: u64,
}

/// Client watchlist replacement.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SetWatchlist {
    /// Full replacement watchlist. Tag 1.
    #[prost(string, repeated, tag = "1")]
    pub markets: Vec<String>,
    /// Workspace revision the client based this change on. Tag 2.
    #[prost(uint64, tag = "2")]
    pub workspace_revision: u64,
}

/// Subscribe to a view's snapshot and delta stream.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct SubscribeView {
    /// View as a [`ViewKind`] value. Tag 1.
    #[prost(enumeration = "ViewKind", tag = "1")]
    pub view: i32,
}

/// Unsubscribe from a view's stream.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct UnsubscribeView {
    /// View as a [`ViewKind`] value. Tag 1.
    #[prost(enumeration = "ViewKind", tag = "1")]
    pub view: i32,
}

/// One catalog chunk; chunks for one revision reassemble in any order.
#[derive(Clone, PartialEq, prost::Message)]
pub struct CatalogSnapshot {
    /// Catalog revision all chunks of this transfer share. Tag 1.
    #[prost(uint64, tag = "1")]
    pub revision: u64,
    /// Zero-based index of this chunk. Tag 2.
    #[prost(uint32, tag = "2")]
    pub chunk_index: u32,
    /// Total number of chunks in this transfer. Tag 3.
    #[prost(uint32, tag = "3")]
    pub chunk_count: u32,
    /// Entries carried by this chunk. Tag 4.
    #[prost(message, repeated, tag = "4")]
    pub entries: Vec<CatalogEntry>,
}

/// One tradable instrument in the catalog.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct CatalogEntry {
    /// Provider product identifier. Tag 1.
    #[prost(string, tag = "1")]
    pub product_id: String,
    /// Base currency code. Tag 2.
    #[prost(string, tag = "2")]
    pub base_currency: String,
    /// Quote currency code. Tag 3.
    #[prost(string, tag = "3")]
    pub quote_currency: String,
}

/// Full chart snapshot. `payload` is coordinator-encoded bars and is opaque to
/// the engine and this protocol.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ChartSnapshot {
    /// Market these bars belong to. Tag 1.
    #[prost(string, tag = "1")]
    pub market: String,
    /// Bar interval in seconds. Tag 2.
    #[prost(uint32, tag = "2")]
    pub interval_seconds: u32,
    /// Engine epoch fencing this snapshot. Tag 3.
    #[prost(uint64, tag = "3")]
    pub engine_epoch: u64,
    /// Selection generation fencing this snapshot. Tag 4.
    #[prost(uint64, tag = "4")]
    pub selection_generation: u64,
    /// Provider generation fencing this snapshot. Tag 5.
    #[prost(uint64, tag = "5")]
    pub provider_generation: u64,
    /// Coordinator-encoded bars. Tag 6.
    #[prost(bytes = "vec", tag = "6")]
    pub payload: Vec<u8>,
}

/// Incremental chart update; same fencing and opacity rules as [`ChartSnapshot`].
#[derive(Clone, PartialEq, prost::Message)]
pub struct ChartDelta {
    /// Market these bars belong to. Tag 1.
    #[prost(string, tag = "1")]
    pub market: String,
    /// Bar interval in seconds. Tag 2.
    #[prost(uint32, tag = "2")]
    pub interval_seconds: u32,
    /// Engine epoch fencing this delta. Tag 3.
    #[prost(uint64, tag = "3")]
    pub engine_epoch: u64,
    /// Selection generation fencing this delta. Tag 4.
    #[prost(uint64, tag = "4")]
    pub selection_generation: u64,
    /// Provider generation fencing this delta. Tag 5.
    #[prost(uint64, tag = "5")]
    pub provider_generation: u64,
    /// Coordinator-encoded bars. Tag 6.
    #[prost(bytes = "vec", tag = "6")]
    pub payload: Vec<u8>,
}

/// Depth-of-market snapshot; `payload` is coordinator-encoded and opaque.
#[derive(Clone, PartialEq, prost::Message)]
pub struct DomSnapshot {
    /// Market this book belongs to. Tag 1.
    #[prost(string, tag = "1")]
    pub market: String,
    /// Engine epoch fencing this snapshot. Tag 2.
    #[prost(uint64, tag = "2")]
    pub engine_epoch: u64,
    /// Selection generation fencing this snapshot. Tag 3.
    #[prost(uint64, tag = "3")]
    pub selection_generation: u64,
    /// Provider generation fencing this snapshot. Tag 4.
    #[prost(uint64, tag = "4")]
    pub provider_generation: u64,
    /// Coordinator-encoded book. Tag 5.
    #[prost(bytes = "vec", tag = "5")]
    pub payload: Vec<u8>,
}

/// Provider connection state transition.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct ProviderState {
    /// State as a [`ProviderConnectionState`] value. Tag 1.
    #[prost(enumeration = "ProviderConnectionState", tag = "1")]
    pub state: i32,
    /// Monotonic provider generation for this transition. Tag 2.
    #[prost(uint64, tag = "2")]
    pub generation: u64,
}

/// Engine fault report; `redacted_detail` must never contain secrets.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Fault {
    /// Fault code as an [`EngineFaultCode`] value. Tag 1.
    #[prost(enumeration = "EngineFaultCode", tag = "1")]
    pub code: i32,
    /// Redacted human-readable detail. Tag 2.
    #[prost(string, tag = "2")]
    pub redacted_detail: String,
}

/// Single-instance coordination: tells a second UI instance to yield and the
/// first instance to activate. Tag-less empty message.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct ActivateExistingUi {}

/// Orderly connection shutdown notice.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Goodbye {
    /// Human-readable shutdown reason. Tag 1.
    #[prost(string, tag = "1")]
    pub reason: String,
}

/// Versioned wrapper carried in every frame.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Envelope {
    /// Protocol version of this envelope; must equal [`crate::PROTOCOL_VERSION`]. Tag 1.
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Versioned message payload. Tags 2 through 17.
    #[prost(
        oneof = "envelope::Payload",
        tags = "2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17"
    )]
    pub payload: Option<envelope::Payload>,
}

/// Payload variants for [`Envelope`].
pub mod envelope {
    /// Versioned message payload; tags are stable and append-only.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        /// Client handshake. Tag 2.
        #[prost(message, tag = "2")]
        ClientHello(super::ClientHello),
        /// Engine handshake reply. Tag 3.
        #[prost(message, tag = "3")]
        EngineReady(super::EngineReady),
        /// Workspace restore request. Tag 4.
        #[prost(message, tag = "4")]
        RestoreWorkspace(super::RestoreWorkspace),
        /// Workspace state reply. Tag 5.
        #[prost(message, tag = "5")]
        WorkspaceState(super::WorkspaceState),
        /// Selection change request. Tag 6.
        #[prost(message, tag = "6")]
        SetSelection(super::SetSelection),
        /// Watchlist replacement request. Tag 7.
        #[prost(message, tag = "7")]
        SetWatchlist(super::SetWatchlist),
        /// View subscription. Tag 8.
        #[prost(message, tag = "8")]
        SubscribeView(super::SubscribeView),
        /// View unsubscription. Tag 9.
        #[prost(message, tag = "9")]
        UnsubscribeView(super::UnsubscribeView),
        /// Catalog chunk. Tag 10.
        #[prost(message, tag = "10")]
        CatalogSnapshot(super::CatalogSnapshot),
        /// Chart snapshot. Tag 11.
        #[prost(message, tag = "11")]
        ChartSnapshot(super::ChartSnapshot),
        /// Chart delta. Tag 12.
        #[prost(message, tag = "12")]
        ChartDelta(super::ChartDelta),
        /// DOM snapshot. Tag 13.
        #[prost(message, tag = "13")]
        DomSnapshot(super::DomSnapshot),
        /// Provider state transition. Tag 14.
        #[prost(message, tag = "14")]
        ProviderState(super::ProviderState),
        /// Fault report. Tag 15.
        #[prost(message, tag = "15")]
        Fault(super::Fault),
        /// Single-instance activation. Tag 16.
        #[prost(message, tag = "16")]
        ActivateExistingUi(super::ActivateExistingUi),
        /// Shutdown notice. Tag 17.
        #[prost(message, tag = "17")]
        Goodbye(super::Goodbye),
    }
}
