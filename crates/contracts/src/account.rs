//! Bounded in-process account commands, replies, and sanitized views.
//!
//! The account runtime owns PKCE transactions, loopback callbacks, token
//! exchange, and vault material. Desktop presentation receives only browser
//! authorization metadata and sanitized account state.

/// Runtime-owned account session state shared by all desktop windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum AccountSessionState {
    #[default]
    SignedOut = 0,
    Authorizing = 1,
    Active = 2,
    OfflineLease = 3,
    ReauthenticationRequired = 4,
    LeaseExpired = 5,
    TerminalError = 6,
}

/// Starts one generation-fenced native login transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BeginLogin {
    pub client_id: u64,
    pub request_generation: u64,
}

/// Cancels one pending login transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CancelLogin {
    pub request_generation: u64,
}

/// Requests the current sanitized account view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GetAccountStatus {}

/// Requests one runtime-owned background refresh of the current profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefreshAccountProfile {
    pub request_id: u64,
}

/// Signs out one runtime-owned account session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignOut {}

/// Runtime-issued browser authorization address for one login transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginAuthorization {
    pub request_generation: u64,
    pub authorization_url: String,
    pub expires_unix_seconds: u64,
}

/// Sanitized account state safe for desktop rendering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountView {
    pub state: AccountSessionState,
    pub account_id: String,
    pub plan_id: String,
    pub detail: String,
    pub request_generation: u64,
    pub display_name: String,
    pub email: String,
    pub photo_url: String,
}
