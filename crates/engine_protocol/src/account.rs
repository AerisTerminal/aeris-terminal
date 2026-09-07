//! Bounded versioned account commands, replies, and sanitized views.
//!
//! The desktop sends `BeginLogin` over authenticated IPC. The engine owns the
//! PKCE transaction, loopback callback, token exchange, and vault material,
//! and the desktop receives only the authorization URL plus sanitized
//! `AccountView` state. No secret material, contact details, vendor
//! identifiers, or purchase records cross this boundary.

/// Engine-owned account session state shared by all desktop windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum AccountSessionState {
    SignedOut = 0,
    Authorizing = 1,
    Active = 2,
    OfflineLease = 3,
    ReauthenticationRequired = 4,
    LeaseExpired = 5,
    TerminalError = 6,
}

/// Starts one generation-fenced native login transaction.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct BeginLogin {
    /// Stable client identity for this desktop lifetime.
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
    /// Monotonic desktop login-request generation; retired results are dropped.
    #[prost(uint64, tag = "2")]
    pub request_generation: u64,
}

/// Cancels one pending login transaction.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct CancelLogin {
    /// Login-request generation being cancelled.
    #[prost(uint64, tag = "1")]
    pub request_generation: u64,
}

/// Requests the current sanitized account view.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct GetAccountStatus {}

/// Requests one engine-owned background refresh of the current account
/// profile. The command carries no account or provider identity; the engine
/// derives and generation-fences all identity from its verified session.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct RefreshAccountProfile {
    /// Non-secret request marker. A non-zero field keeps the command explicit
    /// on the wire while carrying no user/provider identity.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
}

/// Signs out one engine-owned account session.
///
/// Clears the session state and deletes vault refresh and lease material on
/// every engine. The device key stays: it identifies the device, not the
/// user. Complete uninstall removes it separately.
#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct SignOut {}

/// Engine-issued browser authorization address for one login transaction.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct LoginAuthorization {
    /// Login-request generation this URL belongs to.
    #[prost(uint64, tag = "1")]
    pub request_generation: u64,
    /// Axiusflow authentication URL the desktop opens in the system browser.
    #[prost(string, tag = "2")]
    pub authorization_url: String,
    /// Loopback-transaction expiry in Unix seconds.
    #[prost(uint64, tag = "3")]
    pub expires_unix_seconds: u64,
}

/// Sanitized account state safe for desktop rendering. Carries no secret
/// material, vendor identifiers, or purchase records. Profile display fields
/// come from the control-plane user record verified at link time; the
/// desktop never supplies identity proof, only renders it.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct AccountView {
    /// Current session state.
    #[prost(enumeration = "AccountSessionState", tag = "1")]
    pub state: i32,
    /// Canonical Axiusflow account identity; empty when signed out or authorizing.
    #[prost(string, tag = "2")]
    pub account_id: String,
    /// Internal plan identity; empty when unknown. Vendor price IDs never appear here.
    #[prost(string, tag = "3")]
    pub plan_id: String,
    /// Redacted human-readable detail.
    #[prost(string, tag = "4")]
    pub detail: String,
    /// Latest login-request generation observed by the engine.
    #[prost(uint64, tag = "5")]
    pub request_generation: u64,
    /// Display name from the verified user record; empty when unknown.
    #[prost(string, tag = "6")]
    pub display_name: String,
    /// Email from the verified user record; empty when unknown.
    #[prost(string, tag = "7")]
    pub email: String,
    /// Photo URL from the verified user record; empty when absent.
    #[prost(string, tag = "8")]
    pub photo_url: String,
}
