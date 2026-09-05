//! Desktop-owned account presentation and IPC actions.
//!
//! The desktop sends bounded IPC commands and opens system-browser URLs
//! supplied by the engine. It owns no cloud HTTP client, refresh token,
//! payment secret, entitlement truth, or persistent identity data. Browser
//! opening runs on the account worker thread; the UI thread only polls
//! presentation state.
//!
//! One [`DesktopAccount`] session is shared by all desktop windows, matching
//! the engine-owned session. It owns a single background worker thread with
//! bounded channels, mirroring the lifecycle client.

use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

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

/// Presentation snapshot used when no account session is installed.
#[must_use]
pub fn unavailable_presentation() -> AccountPresentation {
    AccountPresentation {
        action: "Sign in",
        state: "Sign-in unavailable",
        plan: "No plan",
        detail: String::new(),
        pending: false,
    }
}

/// Owned settings-menu state for one account session.
#[derive(Clone, Debug)]
pub struct AccountMenuState {
    /// Row presentation snapshot.
    pub presentation: AccountPresentation,
    /// Latest redacted account error, if any.
    pub error: Option<String>,
}

/// Menu state used when no account session is installed.
#[must_use]
pub fn unavailable_menu_state() -> AccountMenuState {
    AccountMenuState {
        presentation: unavailable_presentation(),
        error: None,
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

/// Bounded account worker command.
#[derive(Clone, Copy, Debug)]
enum AccountRequest {
    BeginLogin { client_id: u64, generation: u64 },
    CancelLogin { generation: u64 },
    RefreshStatus,
    SignOut,
}

/// Bounded account worker outcome.
#[derive(Debug)]
enum AccountResponse {
    Authorized,
    Status(AccountView),
    Cancelled(AccountView),
    SignedOut(AccountView),
}

/// Presentation snapshot for account rendering.
#[derive(Clone, Debug)]
pub struct AccountPresentation {
    /// Action label for the settings row.
    pub action: &'static str,
    /// State label for the settings row.
    pub state: &'static str,
    /// Plan label for the settings row.
    pub plan: &'static str,
    /// Redacted engine detail for the settings row.
    pub detail: String,
    /// Whether an IPC request is in flight.
    pub pending: bool,
}

struct AccountShared {
    client_id: u64,
    generation: AtomicU64,
    pending: AtomicBool,
    version: AtomicU64,
    view: Mutex<AccountView>,
    error: Mutex<Option<String>>,
    last_status_poll: Mutex<Instant>,
    last_seen_version: Mutex<u64>,
    requests: SyncSender<AccountRequest>,
}

fn signed_out_view() -> AccountView {
    AccountView {
        state: AccountSessionState::SignedOut as i32,
        account_id: String::new(),
        plan_id: String::new(),
        detail: String::new(),
        request_generation: 0,
    }
}

/// One desktop-owned account session shared by all windows.
#[derive(Clone)]
pub struct DesktopAccount {
    shared: Arc<AccountShared>,
}

static INSTALLED_ACCOUNT: OnceLock<DesktopAccount> = OnceLock::new();
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// How often the UI refreshes the engine account view while authorizing.
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);

impl DesktopAccount {
    /// Installs the shared account session once per desktop process.
    ///
    /// The first install wins; subsequent calls return the installed
    /// session unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the background account client cannot start.
    pub fn install(client_id: u64) -> Result<Self, String> {
        if let Some(installed) = INSTALLED_ACCOUNT.get() {
            return Ok(installed.clone());
        }
        let _guard = INSTALL_LOCK
            .lock()
            .map_err(|_| "account session install failed".to_string())?;
        if let Some(installed) = INSTALLED_ACCOUNT.get() {
            return Ok(installed.clone());
        }
        let session = Self::spawn(client_id)?;
        let _ = INSTALLED_ACCOUNT.set(session);
        INSTALLED_ACCOUNT
            .get()
            .cloned()
            .ok_or_else(|| "account session install failed".to_string())
    }

    /// Returns the installed shared session, if any.
    #[must_use]
    pub fn shared() -> Option<Self> {
        INSTALLED_ACCOUNT.get().cloned()
    }

    fn spawn(client_id: u64) -> Result<Self, String> {
        let (request_tx, request_rx) = mpsc::sync_channel(2);
        let (result_tx, result_rx) = mpsc::sync_channel(2);
        std::thread::Builder::new()
            .name("axiusflow-account-client".to_string())
            .spawn(move || run_account_client(&request_rx, &result_tx))
            .map_err(|_| "desktop account client could not start".to_string())?;
        let shared = Arc::new(AccountShared {
            client_id,
            generation: AtomicU64::new(0),
            pending: AtomicBool::new(false),
            version: AtomicU64::new(0),
            view: Mutex::new(signed_out_view()),
            error: Mutex::new(None),
            last_status_poll: Mutex::new(Instant::now()),
            last_seen_version: Mutex::new(0),
            requests: request_tx,
        });
        let poller = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("axiusflow-account-poller".to_string())
            .spawn(move || {
                for response in result_rx {
                    apply_account_response(&poller, response);
                }
            })
            .map_err(|_| "desktop account client could not start".to_string())?;
        Ok(Self { shared })
    }

    /// Returns the owned settings-menu state for rendering.
    #[must_use]
    pub fn menu_state(&self) -> AccountMenuState {
        AccountMenuState {
            presentation: self.presentation(),
            error: self.error(),
        }
    }

    /// Returns the current presentation snapshot for rendering.
    #[must_use]
    pub fn presentation(&self) -> AccountPresentation {
        let view = self
            .shared
            .view
            .lock()
            .map_or_else(|_| signed_out_view(), |view| view.clone());
        AccountPresentation {
            action: account_action_label(&view),
            state: account_state_label(
                AccountSessionState::try_from(view.state).unwrap_or(AccountSessionState::SignedOut),
            ),
            plan: sanitized_plan_label(&view.plan_id),
            detail: view.detail.clone(),
            pending: self.shared.pending.load(Ordering::Acquire),
        }
    }

    /// Returns the latest redacted account error, if any.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared
            .error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    /// Starts one engine-owned login transaction and opens the browser URL.
    ///
    /// # Errors
    ///
    /// Returns an error when a request is already in flight or the worker
    /// cannot be reached. The browser opens on the worker thread.
    pub fn request_sign_in(&self) -> Result<(), String> {
        if self.shared.pending.load(Ordering::Acquire) {
            return Ok(());
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared
            .requests
            .try_send(AccountRequest::BeginLogin {
                client_id: self.shared.client_id,
                generation,
            })
            .map_err(|_| "sign-in request is already pending".to_string())?;
        self.shared.pending.store(true, Ordering::Release);
        if let Ok(mut error) = self.shared.error.lock() {
            error.take();
        }
        Ok(())
    }

    /// Cancels the pending engine-owned login transaction.
    ///
    /// The worker thread stays blocked on the loopback socket until the
    /// browser completes or the engine transaction times out; engine state
    /// clears immediately, so retired callbacks cannot sign the user in.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker cannot be reached.
    pub fn request_cancel(&self) -> Result<(), String> {
        let generation = self.shared.generation.load(Ordering::Acquire);
        self.shared
            .requests
            .try_send(AccountRequest::CancelLogin { generation })
            .map_err(|_| "sign-in cancellation is already pending".to_string())?;
        Ok(())
    }

    /// Signs out the shared engine-owned account session.
    ///
    /// # Errors
    ///
    /// Returns an error when a request is already in flight or the worker
    /// cannot be reached.
    pub fn request_sign_out(&self) -> Result<(), String> {
        if self.shared.pending.load(Ordering::Acquire) {
            return Err("another account request is already pending".to_string());
        }
        self.shared
            .requests
            .try_send(AccountRequest::SignOut)
            .map_err(|_| "an account request is already pending".to_string())?;
        self.shared.pending.store(true, Ordering::Release);
        if let Ok(mut error) = self.shared.error.lock() {
            error.take();
        }
        Ok(())
    }

    /// Applies worker results and refreshes the engine view while
    /// authorizing. Returns whether presentation changed.
    #[must_use]
    pub fn poll(&self) -> bool {
        let mut changed = false;
        let version = self.shared.version.load(Ordering::Acquire);
        if let Ok(mut seen) = self.shared.last_seen_version.lock()
            && *seen != version
        {
            *seen = version;
            changed = true;
        }
        let authorizing = self
            .shared
            .view
            .lock()
            .is_ok_and(|view| view.state == AccountSessionState::Authorizing as i32);
        if authorizing && !self.shared.pending.load(Ordering::Acquire) {
            let due = self
                .shared
                .last_status_poll
                .lock()
                .map_or(true, |last| last.elapsed() >= STATUS_POLL_INTERVAL);
            if due
                && self
                    .shared
                    .requests
                    .try_send(AccountRequest::RefreshStatus)
                    .is_ok()
                && let Ok(mut last) = self.shared.last_status_poll.lock()
            {
                *last = Instant::now();
                changed = true;
            }
        }
        changed
    }
}

fn apply_account_response(shared: &AccountShared, response: Result<AccountResponse, String>) {
    match response {
        Ok(AccountResponse::Authorized) => {
            // The engine holds the Authorizing view; the browser is open.
            // Status polling picks up the outcome.
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        Ok(
            AccountResponse::Status(view)
            | AccountResponse::Cancelled(view)
            | AccountResponse::SignedOut(view),
        ) => {
            shared.pending.store(false, Ordering::Release);
            if let Ok(mut current) = shared.view.lock() {
                *current = view;
            }
            if let Ok(mut error) = shared.error.lock() {
                error.take();
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        Err(error) => {
            shared.pending.store(false, Ordering::Release);
            if let Ok(mut slot) = shared.error.lock() {
                *slot = Some(error);
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
    }
}

fn run_account_client(requests: &Receiver<AccountRequest>, results: &SyncSender<AccountResult>) {
    while let Ok(request) = requests.recv() {
        let response = handle_account_request(request);
        if results.send(response).is_err() {
            return;
        }
    }
}

type AccountResult = Result<AccountResponse, String>;

fn handle_account_request(request: AccountRequest) -> AccountResult {
    let mut client =
        axiusflow_local_engine_client::sibling_engine_executable().and_then(|executable| {
            axiusflow_local_engine_client::connect_or_start_engine(&executable)
        })?;
    match request {
        AccountRequest::BeginLogin {
            client_id,
            generation,
        } => {
            // The engine holds the Authorizing view and expiry; the browser
            // is already open on this thread.
            let _ = start_login(&mut client, client_id, generation)?;
            Ok(AccountResponse::Authorized)
        }
        AccountRequest::CancelLogin { generation } => {
            let view = cancel_login(&mut client, generation)?;
            Ok(AccountResponse::Cancelled(view))
        }
        AccountRequest::RefreshStatus => {
            let view = fetch_account_status(&mut client)?;
            Ok(AccountResponse::Status(view))
        }
        AccountRequest::SignOut => {
            let view = sign_out(&mut client)?;
            Ok(AccountResponse::SignedOut(view))
        }
    }
}

/// Signs out the shared engine-owned session.
///
/// # Errors
///
/// Returns an error when the request fails or the reply is invalid.
pub fn sign_out(client: &mut EngineClient) -> Result<AccountView, String> {
    client.sign_out()
}

#[cfg(test)]
mod tests {
    use super::{
        DesktopAccount, account_action_label, account_state_label, sanitized_plan_label,
        unavailable_menu_state,
    };
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

    #[test]
    fn unavailable_menu_state_invites_sign_in() {
        let menu = unavailable_menu_state();
        assert_eq!(menu.presentation.action, "Sign in");
        assert_eq!(menu.presentation.state, "Sign-in unavailable");
        assert!(menu.error.is_none());
    }

    #[test]
    fn installed_session_starts_signed_out_and_poll_is_quiet() {
        // Install spawns worker threads but performs no IPC until requested.
        let session = DesktopAccount::install(u64::from(std::process::id()))
            .expect("account session installs");
        assert!(!session.poll());
        let presentation = session.presentation();
        assert_eq!(presentation.action, "Sign in");
        assert!(!presentation.pending);
        assert!(session.error().is_none());
        let menu = session.menu_state();
        assert_eq!(menu.presentation.action, "Sign in");
    }
}
