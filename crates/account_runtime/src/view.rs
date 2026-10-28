//! Sanitized account views the account runtime publishes to the desktop.
//!
//! The runtime owns PKCE transactions, loopback callbacks, token exchange and
//! vault material; presentation receives only browser authorization metadata
//! and sanitized account state.

/// Runtime-owned account session state shared by all desktop windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum AccountSessionState {
    #[default]
    SignedOut = 0,
    Authorizing = 1,
    Active = 2,
    /// Signed in from the cached verified profile while the identity
    /// provider has not confirmed the session or is unreachable.
    Offline = 3,
    ReauthenticationRequired = 4,
    TerminalError = 5,
}

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

#[cfg(test)]
mod tests {
    use super::AccountSessionState;

    #[test]
    fn account_session_states_have_stable_values() {
        assert_eq!(AccountSessionState::SignedOut as i32, 0);
        assert_eq!(AccountSessionState::Authorizing as i32, 1);
        assert_eq!(AccountSessionState::Active as i32, 2);
        assert_eq!(AccountSessionState::Offline as i32, 3);
        assert_eq!(AccountSessionState::ReauthenticationRequired as i32, 4);
        assert_eq!(AccountSessionState::TerminalError as i32, 5);
    }
}
