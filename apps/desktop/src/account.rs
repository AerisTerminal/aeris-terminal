//! Desktop-owned account presentation and IPC actions.
//!
//! The desktop sends bounded IPC commands and opens system-browser URLs
//! supplied by the engine. It owns no cloud HTTP client, refresh token,
//! payment secret, entitlement truth, or persistent identity data. Browser
//! opening runs on the calling background thread; callers must keep it off
//! the UI thread.

use axiusflow_engine_protocol::{AccountSessionState, AccountView, LoginAuthorization};
use axiusflow_local_engine_client::EngineClient;

/// Starts one engine-owned login transaction and opens the returned
/// authorization URL in the system browser on a background thread.
///
/// Returns the engine authorization reply for expiry display. The browser
/// launch is fire-and-forget: a spawn failure is reported, but later
/// browser exit status is not tracked.
///
/// # Errors
///
/// Returns an error when the IPC transaction fails or no system browser
/// can be launched.
pub fn start_login(
    client: &mut EngineClient,
    client_id: u64,
    request_generation: u64,
) -> Result<LoginAuthorization, String> {
    let authorization = client.begin_login(client_id, request_generation)?;
    let url = authorization.authorization_url.clone();
    if url.is_empty() || url.len() > axiusflow_platform_runtime::MAXIMUM_AUTHORIZATION_URL_BYTES {
        return Err("engine returned an invalid authorization URL".to_string());
    }
    std::thread::Builder::new()
        .name("axiusflow-open-browser".to_string())
        .spawn(move || {
            if let Err(error) = axiusflow_platform_runtime::open_system_browser(&url) {
                eprintln!("Axiusflow browser open degraded: {error}");
            }
        })
        .map_err(|_| "system browser could not be opened".to_string())?;
    Ok(authorization)
}

/// Returns the current sanitized engine-owned account view.
///
/// # Errors
///
/// Returns an error when the IPC request fails or the reply is invalid.
pub fn fetch_account_status(client: &mut EngineClient) -> Result<AccountView, String> {
    client.account_status()
}

/// Verifies one sanitized account view for the lifecycle readiness probe.
///
/// # Errors
///
/// Returns an error when the session state is not a known protocol value.
pub fn verify_account_readiness(view: &AccountView) -> Result<(), String> {
    AccountSessionState::try_from(view.state)
        .map(|_| ())
        .map_err(|_| "candidate account service did not reach readiness".to_string())
}

/// Cancels one pending engine-owned login transaction.
///
/// # Errors
///
/// Returns an error when no matching transaction is pending.
pub fn cancel_login(
    client: &mut EngineClient,
    request_generation: u64,
) -> Result<AccountView, String> {
    client.cancel_login(request_generation)
}

/// Human-readable label for one account session state.
#[must_use]
pub const fn account_state_label(state: AccountSessionState) -> &'static str {
    match state {
        AccountSessionState::SignedOut => "Signed out",
        AccountSessionState::Authorizing => "Waiting for browser sign-in",
        AccountSessionState::Active => "Signed in",
        AccountSessionState::OfflineLease => "Signed in (offline)",
        AccountSessionState::ReauthenticationRequired => "Sign-in required",
        AccountSessionState::LeaseExpired => "Subscription expired",
        AccountSessionState::TerminalError => "Sign-in unavailable",
    }
}

/// Human-readable action for one sanitized account view.
#[must_use]
pub fn account_action_label(view: &AccountView) -> &'static str {
    match AccountSessionState::try_from(view.state) {
        Ok(AccountSessionState::Active | AccountSessionState::OfflineLease) => "Account",
        Ok(AccountSessionState::Authorizing) => "Waiting for browser",
        Ok(_) | Err(_) => "Sign in",
    }
}

/// Human-readable plan label for one sanitized internal plan identity.
/// Unknown values stay generic: vendor price IDs never reach this boundary.
#[must_use]
pub fn sanitized_plan_label(plan_id: &str) -> &'static str {
    match plan_id {
        "" => "No plan",
        "starter" => "Starter",
        "pro" => "Pro",
        "elite" => "Elite",
        "enterprise" => "Enterprise",
        _ => "Unknown plan",
    }
}

#[cfg(test)]
mod tests {
    use super::{account_action_label, account_state_label, sanitized_plan_label};
    use axiusflow_engine_protocol::{AccountSessionState, AccountView};

    fn view(state: AccountSessionState) -> AccountView {
        AccountView {
            state: state as i32,
            account_id: String::new(),
            plan_id: String::new(),
            detail: String::new(),
            request_generation: 1,
        }
    }

    #[test]
    fn every_session_state_has_a_stable_label() {
        assert_eq!(
            account_state_label(AccountSessionState::SignedOut),
            "Signed out"
        );
        assert_eq!(
            account_state_label(AccountSessionState::Authorizing),
            "Waiting for browser sign-in"
        );
        assert_eq!(
            account_state_label(AccountSessionState::Active),
            "Signed in"
        );
        assert_eq!(
            account_state_label(AccountSessionState::OfflineLease),
            "Signed in (offline)"
        );
        assert_eq!(
            account_state_label(AccountSessionState::ReauthenticationRequired),
            "Sign-in required"
        );
        assert_eq!(
            account_state_label(AccountSessionState::LeaseExpired),
            "Subscription expired"
        );
        assert_eq!(
            account_state_label(AccountSessionState::TerminalError),
            "Sign-in unavailable"
        );
    }

    #[test]
    fn action_labels_match_recovery_expectations() {
        assert_eq!(
            account_action_label(&view(AccountSessionState::SignedOut)),
            "Sign in"
        );
        assert_eq!(
            account_action_label(&view(AccountSessionState::Authorizing)),
            "Waiting for browser"
        );
        assert_eq!(
            account_action_label(&view(AccountSessionState::Active)),
            "Account"
        );
        assert_eq!(
            account_action_label(&view(AccountSessionState::LeaseExpired)),
            "Sign in"
        );
        let mut unknown = view(AccountSessionState::SignedOut);
        unknown.state = 99;
        assert_eq!(account_action_label(&unknown), "Sign in");
    }

    #[test]
    fn plan_labels_stay_sanitized() {
        assert_eq!(sanitized_plan_label(""), "No plan");
        assert_eq!(sanitized_plan_label("pro"), "Pro");
        assert_eq!(sanitized_plan_label("price_123"), "Unknown plan");
    }
}
