//! Capability-oriented operating-system boundary for native clients and ingest hosts.
//!
//! Consumers depend on capabilities rather than branching on operating-system names.

pub mod browser;
mod capability;
mod credential_vault;
pub mod data_root;
mod dns;
mod durable_container;
mod file_manager;
mod http_client;
mod io_cancellation;
mod live_market_gate;
mod market_socket;
mod network_notifications;
mod power_notifications;
mod user_notifications;

pub use aeris_platform_sys::fs;
pub use aeris_platform_sys::fs::replace_file as replace_file_atomically;
pub use aeris_platform_sys::process;
#[cfg(target_os = "windows")]
pub use aeris_platform_sys::session::{
    NativeSessionShutdownGuard, NativeSessionShutdownPermit, SessionShutdownError,
};
#[cfg(target_os = "windows")]
pub use aeris_platform_sys::trust;
pub use aeris_platform_sys::ui::{confirm_message_box, message_box};
pub use browser::{MAXIMUM_AUTHORIZATION_URL_BYTES, open_system_browser};
pub use capability::CapabilityAvailability;
pub use credential_vault::{CredentialVault, NativeCredentialVault, NativeCredentialVaultError};
pub use data_root::{
    DataRoot, DataRootError, DataRootMode, InstalledAppSideEffect, InstanceLock,
    ParsedDataRootArguments, active_data_root, initialize_data_root,
    migrate_legacy_native_data_root, native_data_root, parse_data_root_arguments,
};
pub use dns::resolve_addresses;
pub use durable_container::{
    DURABLE_CONTAINER_MAGIC, DURABLE_CONTAINER_VERSION, DurableCandidate, DurableContainer,
    DurableFormat, DurableLoad, DurableNewerVersion, DurableRejection, DurableSource,
    MAXIMUM_QUARANTINED_COPIES,
};
pub use file_manager::reveal_in_file_manager;
pub use http_client::CancellableHttpClient;
pub use io_cancellation::cancel_tcp_stream_io;
pub use live_market_gate::{
    LIVE_MARKET_GATE_EVIDENCE_SCOPE, LIVE_MARKET_GATE_MAXIMUM_BINARY_BYTES,
    LIVE_MARKET_GATE_MAXIMUM_DETAIL_BYTES, LIVE_MARKET_GATE_MAXIMUM_REPORT_BYTES,
    LIVE_MARKET_GATE_SCHEMA_VERSION, LiveMarketGateCompletion, LiveMarketGateEvidence,
    LiveMarketGateOutcome, LiveMarketGateRecorder,
};
pub use market_socket::{
    MAXIMUM_MARKET_SOCKET_MESSAGE_BYTES, MarketSocket, MarketSocketError, MarketSocketEvent,
    MarketSocketShutdown,
};
pub use network_notifications::{
    NativeNetworkMonitor, NativeNetworkMonitorCancellation, NetworkEvent, NetworkNotificationError,
};
pub use power_notifications::{
    NativePowerMonitor, NativePowerMonitorCancellation, PowerEvent, PowerNotificationError,
};
pub use user_notifications::{
    APPLICATION_IDENTIFIER, APPLICATION_NAME, NativeUserNotification, NativeUserNotificationError,
    try_send_user_notification,
};
