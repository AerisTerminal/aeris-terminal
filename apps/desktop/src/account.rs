//! Desktop-owned account presentation over one in-process account runtime.
//!
//! Network and credential work stays off the GPUI thread, but there is no
//! secondary process, local transport, or reconnect/replay layer.

use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

use axiusflow_account_runtime::{AccountService, AccountServiceConfig};
use axiusflow_contracts::{AccountSessionState, AccountView, LoginAuthorization};

/// Production account hub used by the native Manage Profile action.
pub const MANAGE_PROFILE_URL: &str = "https://auth.axiusflow.com/account?section=profile";

/// Opens the production account hub on a background thread. The GPUI thread
/// never performs browser/process work.
///
/// # Errors
///
/// Returns an error when the background browser worker cannot be started.
pub fn open_manage_profile() -> Result<(), String> {
    std::thread::Builder::new()
        .name("axiusflow-open-profile".to_string())
        .spawn(|| {
            if let Err(error) = axiusflow_platform_runtime::open_system_browser(MANAGE_PROFILE_URL)
            {
                eprintln!("Axiusflow profile browser open degraded: {error}");
            }
        })
        .map(|_| ())
        .map_err(|_| "profile page could not be opened".to_string())
}

/// Starts one runtime-owned login transaction and opens the returned
/// authorization URL in the system browser on a background thread.
///
/// Returns the runtime authorization reply for expiry display. The browser
/// launch runs bounded off-thread: a missing launcher, a nonzero launcher
/// exit, or a hung launcher is reported, and a hung launcher is killed.
///
/// # Errors
///
/// Returns an error when the account-runtime request fails or no system browser
/// can be launched.
pub fn start_login(
    service: &AccountService,
    request_generation: u64,
) -> Result<LoginAuthorization, String> {
    let authorization = service.begin_login(request_generation)?;
    let url = authorization.authorization_url.clone();
    if url.is_empty() || url.len() > axiusflow_platform_runtime::MAXIMUM_AUTHORIZATION_URL_BYTES {
        return Err("account service returned an invalid authorization URL".to_string());
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

/// Returns the current sanitized runtime-owned account view.
///
/// # Errors
///
/// Returns an error when the account runtime request fails or the reply is invalid.
pub fn fetch_account_status(service: &AccountService) -> Result<AccountView, String> {
    Ok(service.account_status())
}

/// Cancels one pending engine-owned login transaction.
///
/// # Errors
///
/// Returns an error when no matching transaction is pending.
pub fn cancel_login(
    service: &AccountService,
    request_generation: u64,
) -> Result<AccountView, String> {
    service.cancel_login(request_generation)?;
    Ok(service.account_status())
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
        AccountSessionState::LeaseExpired => "Sign-in expired",
        AccountSessionState::TerminalError => "Sign-in unavailable",
    }
}

/// Human-readable action for one sanitized account view.
#[must_use]
pub fn account_action_label(view: &AccountView) -> &'static str {
    match view.state {
        AccountSessionState::Active | AccountSessionState::OfflineLease => "Account",
        AccountSessionState::Authorizing => "Waiting for browser",
        _ => "Sign in",
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
        display_name: String::new(),
        email: String::new(),
        photo_url: String::new(),
        pending: false,
    }
}

/// Derives avatar initials from the verified profile. The first
/// alphanumeric character of up to two name parts wins; the email local
/// part backs it up. Never empty, so the avatar always has content when
/// the photo is absent or fails to load.
#[must_use]
pub fn profile_initials(display_name: &str, email: &str) -> String {
    let mut initials = String::new();
    for part in display_name.split_whitespace() {
        if let Some(first) = part.chars().find(|ch| ch.is_alphanumeric()) {
            initials.push(first);
            if initials.chars().count() >= 2 {
                break;
            }
        }
    }
    if initials.is_empty() {
        let local = email.split('@').next().unwrap_or("");
        for first in local.chars().filter(|ch| ch.is_alphanumeric()).take(2) {
            initials.push(first);
        }
    }
    if initials.is_empty() {
        initials.push('A');
    }
    initials.to_uppercase()
}

/// Returns whether a profile photo URL is renderable. Only `https` URLs
/// reach the image loader; anything else falls back to initials.
#[must_use]
pub fn has_profile_photo(photo_url: &str) -> bool {
    !photo_url.is_empty()
        && photo_url.len() <= 2048
        && photo_url.starts_with("https://")
        && !photo_url.contains([' ', '\n', '\r', '\t'])
}

/// Owned settings-menu state for one account session.
#[derive(Clone, Debug)]
pub struct AccountMenuState {
    /// Row presentation snapshot.
    pub presentation: AccountPresentation,
    /// Latest redacted account error, if any.
    pub error: Option<String>,
}

impl AccountMenuState {
    /// Returns whether the session is signed in.
    #[must_use]
    pub fn signed_in(&self) -> bool {
        self.presentation.action == "Account"
    }

    /// Returns whether the session is waiting on the browser.
    #[must_use]
    pub fn authorizing(&self) -> bool {
        self.presentation.action == "Waiting for browser"
    }

    /// Returns whether the session is signed in on a cached lease.
    #[must_use]
    pub fn offline(&self) -> bool {
        self.signed_in()
            && self.presentation.state == account_state_label(AccountSessionState::OfflineLease)
    }

    /// Returns whether the panel hides identity metadata. A plain signed-out
    /// session keeps signed-out status and plan metadata hidden; global menu
    /// actions may still render. Every other state names itself so the trader
    /// knows what happened.
    #[must_use]
    pub fn hides_identity(&self) -> bool {
        !self.signed_in()
            && !self.authorizing()
            && self.presentation.state == account_state_label(AccountSessionState::SignedOut)
    }
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
        "starter" => "Early access",
        "pro" => "Pro",
        "elite" => "Elite",
        "enterprise" => "Enterprise",
        _ => "Unknown plan",
    }
}

/// Bounded account worker command.
#[derive(Clone, Copy, Debug)]
enum AccountRequest {
    BeginLogin { generation: u64 },
    CancelLogin { generation: u64 },
    RefreshStatus { seq: u64, epoch: u64 },
    RefreshProfile { epoch: u64 },
    SignOut,
}

/// Bounded account worker outcome. Status outcomes echo the fetch slot so
/// cancellation, retries, and sign-out cannot be overwritten by stale
/// results.
#[derive(Debug)]
enum AccountResponse {
    Authorized(LoginAuthorization),
    Status {
        view: AccountView,
        seq: u64,
        epoch: u64,
    },
    StatusFailed {
        seq: u64,
        epoch: u64,
        error: String,
    },
    ProfileRefreshQueued {
        view: AccountView,
        epoch: u64,
    },
    Cancelled(AccountView),
    SignedOut(AccountView),
    Failed(String),
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
    /// Verified display name; empty unless signed in.
    pub display_name: String,
    /// Verified email; empty unless signed in.
    pub email: String,
    /// Verified photo URL; empty unless signed in with a photo.
    pub photo_url: String,
    /// Whether an account-runtime request is in flight.
    pub pending: bool,
}

struct AccountShared {
    generation: AtomicU64,
    pending: AtomicBool,
    version: AtomicU64,
    view: Mutex<AccountView>,
    error: Mutex<Option<String>>,
    last_status_poll: Mutex<Instant>,
    profile_refresh_until: Mutex<Option<Instant>>,
    last_seen_version: Mutex<u64>,
    request_at: Mutex<Instant>,
    authorization_url: Mutex<Option<String>>,
    /// Bumped on every Begin/Cancel/SignOut submission. Status replies
    /// from an older epoch are stale and drop without touching state.
    epoch: AtomicU64,
    /// Whether one status fetch owns the reply slot.
    status_in_flight: AtomicBool,
    /// Sequence of the latest status fetch.
    status_seq: AtomicU64,
    /// False until the first authoritative account-status reply arrives from
    /// the account runtime. The local default `SignedOut` view is not a real
    /// startup authentication result.
    initial_status_resolved: AtomicBool,
    /// Whether a browser transaction is open. Set when the account runtime accepts
    /// the login request, cleared when the view leaves Authorizing.
    login_open: AtomicBool,
    requests: SyncSender<AccountRequest>,
}

/// Bounds how long one request may stay in flight. The account runtime answers
/// each command through a bounded worker path; the browser authorization wait
/// happens independently. Anything longer is a wedged worker, never a slow login.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

fn signed_out_view() -> AccountView {
    AccountView {
        state: AccountSessionState::SignedOut,
        account_id: String::new(),
        plan_id: String::new(),
        detail: String::new(),
        request_generation: 0,
        display_name: String::new(),
        email: String::new(),
        photo_url: String::new(),
    }
}

/// One desktop-owned account session shared by all windows.
#[derive(Clone)]
pub struct DesktopAccount {
    shared: Arc<AccountShared>,
}

static INSTALLED_ACCOUNT: OnceLock<DesktopAccount> = OnceLock::new();
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// How often the engine view refreshes while a browser transaction is open.
/// Fast enough that runtime completion reaches the UI within a frame
/// budget, slow enough to keep one bounded runtime fetch in flight. Idle
/// sessions poll slowly for restore and expiry.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);
const INITIAL_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Briefly poll the engine faster after returning from Manage Profile so a
/// completed engine-owned refresh reaches presentation promptly.
const PROFILE_REFRESH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const PROFILE_REFRESH_POLL_WINDOW: Duration = Duration::from_secs(10);
/// Idle refresh so restored sessions and runtime expiry reach the UI.
const IDLE_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(30);

impl DesktopAccount {
    /// Installs the shared account session once per desktop process.
    ///
    /// The first install wins; subsequent calls return the installed
    /// session unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the background account client cannot start.
    pub fn install() -> Result<Self, String> {
        if let Some(installed) = INSTALLED_ACCOUNT.get() {
            return Ok(installed.clone());
        }
        let _guard = INSTALL_LOCK
            .lock()
            .map_err(|_| "account session install failed".to_string())?;
        if let Some(installed) = INSTALLED_ACCOUNT.get() {
            return Ok(installed.clone());
        }
        let session = Self::spawn()?;
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

    /// Returns whether the engine has verified a usable account session.
    /// The desktop uses this as its hard boundary before creating any
    /// workspace or market worker.
    #[must_use]
    pub fn authenticated(&self) -> bool {
        self.shared.view.lock().is_ok_and(|view| {
            matches!(
                view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        })
    }

    /// Whether startup is still waiting for the account runtime's first
    /// authoritative account status.
    #[must_use]
    pub fn verification_pending(&self) -> bool {
        !self.shared.initial_status_resolved.load(Ordering::Acquire)
            || self.shared.view.lock().is_ok_and(|view| {
                // Generation zero is startup restoration, not a new
                // browser transaction. The first runtime reply can arrive before
                // refresh/link verification finishes.
                view.request_generation == 0 && view.state == AccountSessionState::Authorizing
            })
    }

    fn spawn() -> Result<Self, String> {
        Self::spawn_with(handle_account_request)
    }

    /// Spawns one isolated session with a scripted worker. Production uses
    /// [`handle_account_request`]; tests inject a fake runtime handler.
    fn spawn_with(
        handle: impl Fn(AccountRequest) -> AccountResponse + Send + 'static,
    ) -> Result<Self, String> {
        let (request_tx, request_rx) = mpsc::sync_channel(2);
        let (result_tx, result_rx) = mpsc::sync_channel(2);
        std::thread::Builder::new()
            .name("axiusflow-account-client".to_string())
            .spawn(move || run_account_client_with(&request_rx, &result_tx, handle))
            .map_err(|_| "desktop account client could not start".to_string())?;
        // Generations seed from the wall clock so a fresh desktop process
        // always supersedes generations from a previous process lifetime.
        // Persisted account generations can survive a desktop restart; restarting
        // the counter at zero would make the first sign-in look retired.
        let now = Instant::now();
        let shared = Arc::new(AccountShared {
            generation: AtomicU64::new(unix_millis()),
            pending: AtomicBool::new(false),
            version: AtomicU64::new(0),
            view: Mutex::new(signed_out_view()),
            error: Mutex::new(None),
            last_status_poll: Mutex::new(now),
            profile_refresh_until: Mutex::new(None),
            last_seen_version: Mutex::new(0),
            request_at: Mutex::new(now),
            authorization_url: Mutex::new(None),
            epoch: AtomicU64::new(0),
            status_in_flight: AtomicBool::new(false),
            status_seq: AtomicU64::new(0),
            initial_status_resolved: AtomicBool::new(false),
            login_open: AtomicBool::new(false),
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
        let session = Self { shared };
        // Fetch engine state at startup so a restored session (or an
        // runtime expiry) reaches the UI on the first frames.
        session.queue_status();
        Ok(session)
    }

    /// Queues one generation-fenced status fetch when the reply slot is
    /// free. Best-effort: a full queue retries on the next poll.
    fn queue_status(&self) {
        if self.shared.status_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        let seq = self.shared.status_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        if self
            .shared
            .requests
            .try_send(AccountRequest::RefreshStatus { seq, epoch })
            .is_err()
        {
            self.shared.status_in_flight.store(false, Ordering::Release);
            return;
        }
        if let Ok(mut last) = self.shared.last_status_poll.lock() {
            *last = Instant::now();
        }
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
            state: account_state_label(view.state),
            plan: sanitized_plan_label(&view.plan_id),
            detail: view.detail.clone(),
            display_name: view.display_name.clone(),
            email: view.email.clone(),
            photo_url: view.photo_url.clone(),
            pending: self.shared.pending.load(Ordering::Acquire),
        }
    }

    /// Returns the current verified internal plan identity.
    #[must_use]
    pub fn plan_id(&self) -> String {
        self.shared.view.lock().map_or_else(
            |_| "starter".to_string(),
            |view| {
                match view.state {
                    AccountSessionState::Active | AccountSessionState::OfflineLease => {}
                    _ => return "starter".to_string(),
                }
                match view.plan_id.as_str() {
                    "pro" | "elite" | "enterprise" => view.plan_id.clone(),
                    _ => "starter".to_string(),
                }
            },
        )
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
    /// Repeated clicks collapse into the in-flight request; a click while
    /// a transaction is already open reopens the same browser page instead
    /// of stacking a second transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker cannot be reached. The browser
    /// opens on the worker thread.
    pub fn request_sign_in(&self) -> Result<(), String> {
        if self.shared.pending.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.shared.login_open.load(Ordering::Acquire) {
            return self.reopen_browser();
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.begin_request(
            AccountRequest::BeginLogin { generation },
            "sign-in request is already pending",
        )
    }

    /// Marks one worker request in flight before queue submission and rolls
    /// back when the queue is full, so a fast reply can never land first
    /// and a failed submission never sticks the UI disabled. Failures land
    /// in the menu error with retry enabled.
    fn begin_request(&self, request: AccountRequest, busy: &str) -> Result<(), String> {
        self.shared.epoch.fetch_add(1, Ordering::AcqRel);
        self.shared.pending.store(true, Ordering::Release);
        if let Ok(mut sent) = self.shared.request_at.lock() {
            *sent = Instant::now();
        }
        if self.shared.requests.try_send(request).is_err() {
            self.shared.pending.store(false, Ordering::Release);
            self.fail(busy);
            return Err(busy.to_string());
        }
        if let Ok(mut error) = self.shared.error.lock() {
            error.take();
        }
        Ok(())
    }

    /// Records one actionable failure for the menu error row.
    fn fail(&self, error: &str) {
        if let Ok(mut slot) = self.shared.error.lock() {
            *slot = Some(error.to_string());
        }
        self.shared.version.fetch_add(1, Ordering::AcqRel);
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
        if self.shared.pending.load(Ordering::Acquire) {
            return Err("sign-in cancellation is already pending".to_string());
        }
        let generation = self.shared.generation.load(Ordering::Acquire);
        self.begin_request(
            AccountRequest::CancelLogin { generation },
            "sign-in cancellation is already pending",
        )
    }

    /// Reopens the stored authorization URL when the browser window was
    /// lost before approval. Never starts a new engine transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when no authorization URL is retained or no system
    /// browser can be launched.
    pub fn reopen_browser(&self) -> Result<(), String> {
        let url = self
            .shared
            .authorization_url
            .lock()
            .ok()
            .and_then(|url| url.clone())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| "no sign-in page to reopen; start sign-in again".to_string())?;
        std::thread::Builder::new()
            .name("axiusflow-open-browser".to_string())
            .spawn(move || {
                if let Err(error) = axiusflow_platform_runtime::open_system_browser(&url) {
                    eprintln!("Axiusflow browser open degraded: {error}");
                }
            })
            .map_err(|_| "system browser could not be opened".to_string())?;
        Ok(())
    }

    /// Signs out the shared runtime-owned account session.
    ///
    /// # Errors
    ///
    /// Returns an error when a request is already in flight or the worker
    /// cannot be reached.
    pub fn request_sign_out(&self) -> Result<(), String> {
        if self.shared.pending.load(Ordering::Acquire) {
            return Err("another account request is already pending".to_string());
        }
        self.begin_request(
            AccountRequest::SignOut,
            "an account request is already pending",
        )
    }

    /// Asks the account runtime to refresh the current verified profile on its
    /// background worker. The reply is immediate and carries no
    /// secret/provider identity from the desktop.
    ///
    /// # Errors
    /// Returns an error when no verified account is active or the bounded
    /// desktop worker queue is unavailable.
    pub fn request_profile_refresh(&self) -> Result<(), String> {
        let refreshable = self.shared.view.lock().is_ok_and(|view| {
            matches!(
                view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        });
        if !refreshable {
            return Err("account profile refresh requires a signed-in account".to_string());
        }
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        self.shared
            .requests
            .try_send(AccountRequest::RefreshProfile { epoch })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "account profile refresh is already queued".to_string()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "account profile refresh is unavailable".to_string()
                }
            })
    }

    /// Applies worker results and keeps the engine view fresh. Status
    /// polling runs fast while a browser transaction is open and slowly
    /// when idle, so restored sessions and runtime expiry reach the UI
    /// without unnecessary runtime polling. Returns whether presentation changed.
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
        if self.shared.pending.load(Ordering::Acquire) {
            let expired = self
                .shared
                .request_at
                .lock()
                .is_ok_and(|sent| sent.elapsed() >= REQUEST_TIMEOUT);
            if expired {
                self.shared.pending.store(false, Ordering::Release);
                self.fail("account request timed out; try again");
                changed = true;
            }
        }
        if !self.shared.pending.load(Ordering::Acquire)
            && !self.shared.status_in_flight.load(Ordering::Acquire)
        {
            // An open transaction polls fast even before the first status
            // lands; otherwise polling would wait on the view it is meant
            // to fetch. Idle sessions poll slowly for restore and expiry.
            let transactional = self.shared.login_open.load(Ordering::Acquire)
                || self
                    .shared
                    .view
                    .lock()
                    .is_ok_and(|view| is_authorizing(&view));
            let profile_refresh_active =
                self.shared
                    .profile_refresh_until
                    .lock()
                    .is_ok_and(|mut until| {
                        if until.is_some_and(|deadline| Instant::now() < deadline) {
                            true
                        } else {
                            *until = None;
                            false
                        }
                    });
            let interval = if self.verification_pending() {
                INITIAL_STATUS_POLL_INTERVAL
            } else if transactional {
                STATUS_POLL_INTERVAL
            } else if profile_refresh_active {
                PROFILE_REFRESH_POLL_INTERVAL
            } else {
                IDLE_STATUS_POLL_INTERVAL
            };
            let due = self
                .shared
                .last_status_poll
                .lock()
                .map_or(true, |last| last.elapsed() >= interval);
            if due {
                self.queue_status();
            }
        }
        changed
    }
}

fn apply_account_response(shared: &AccountShared, response: AccountResponse) {
    match response {
        AccountResponse::Authorized(authorization) => {
            // The engine holds the Authorizing view; the browser is open.
            // The URL is retained so a lost browser window can be reopened
            // without starting a second engine transaction. The transaction
            // opens at once and the next poll fetches immediately, so the
            // outcome reaches the UI instead of waiting on an idle interval.
            if let Ok(mut url) = shared.authorization_url.lock() {
                *url = Some(authorization.authorization_url);
            }
            shared.login_open.store(true, Ordering::Release);
            shared.pending.store(false, Ordering::Release);
            if let Ok(mut error) = shared.error.lock() {
                error.take();
            }
            rewind_status_poll(shared);
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        AccountResponse::Status { view, seq, epoch } => {
            if seq != shared.status_seq.load(Ordering::Acquire) {
                return;
            }
            shared.status_in_flight.store(false, Ordering::Release);
            if epoch != shared.epoch.load(Ordering::Acquire) {
                // Cancelled, retried, or signed out since the fetch: the
                // stale result drops without touching current state.
                return;
            }
            shared
                .initial_status_resolved
                .store(true, Ordering::Release);
            if !is_authorizing(&view) {
                shared.login_open.store(false, Ordering::Release);
                if let Ok(mut url) = shared.authorization_url.lock() {
                    url.take();
                }
            }
            if let Ok(mut current) = shared.view.lock() {
                *current = view;
            }
            if let Ok(mut error) = shared.error.lock() {
                error.take();
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        AccountResponse::StatusFailed { seq, epoch, error } => {
            if seq != shared.status_seq.load(Ordering::Acquire) {
                return;
            }
            shared.status_in_flight.store(false, Ordering::Release);
            if epoch != shared.epoch.load(Ordering::Acquire) {
                return;
            }
            // The transaction stays open: polling continues as the automatic
            // retry while Reopen and Cancel stay enabled. The error is
            // actionable and clears on the next good fetch.
            if let Ok(mut slot) = shared.error.lock() {
                *slot = Some(error);
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        AccountResponse::ProfileRefreshQueued { view, epoch } => {
            if epoch != shared.epoch.load(Ordering::Acquire) {
                return;
            }
            if let Ok(mut current) = shared.view.lock() {
                *current = view;
            }
            if let Ok(mut until) = shared.profile_refresh_until.lock() {
                *until = Instant::now().checked_add(PROFILE_REFRESH_POLL_WINDOW);
            }
            rewind_status_poll(shared);
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        AccountResponse::Cancelled(view) | AccountResponse::SignedOut(view) => {
            shared
                .initial_status_resolved
                .store(true, Ordering::Release);
            shared.pending.store(false, Ordering::Release);
            shared.login_open.store(false, Ordering::Release);
            if let Ok(mut url) = shared.authorization_url.lock() {
                url.take();
            }
            if let Ok(mut current) = shared.view.lock() {
                *current = view;
            }
            if let Ok(mut error) = shared.error.lock() {
                error.take();
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
        AccountResponse::Failed(error) => {
            shared.pending.store(false, Ordering::Release);
            if let Ok(mut slot) = shared.error.lock() {
                *slot = Some(error);
            }
            shared.version.fetch_add(1, Ordering::AcqRel);
        }
    }
}

/// Forces the next poll to fetch immediately instead of waiting out the
/// idle interval. Used when a transaction opens mid-idle-cycle.
fn rewind_status_poll(shared: &AccountShared) {
    if let Ok(mut last) = shared.last_status_poll.lock() {
        *last = Instant::now()
            .checked_sub(STATUS_POLL_INTERVAL)
            .unwrap_or_else(Instant::now);
    }
}

fn is_authorizing(view: &AccountView) -> bool {
    view.state == AccountSessionState::Authorizing
}

fn run_account_client_with(
    requests: &Receiver<AccountRequest>,
    results: &SyncSender<AccountResponse>,
    handle: impl Fn(AccountRequest) -> AccountResponse,
) {
    while let Ok(request) = requests.recv() {
        if results.send(handle(request)).is_err() {
            return;
        }
    }
}

fn handle_account_request(request: AccountRequest) -> AccountResponse {
    let service = account_service();
    match request {
        AccountRequest::BeginLogin { generation } => match start_login(service, generation) {
            Ok(authorization) => AccountResponse::Authorized(authorization),
            Err(error) => AccountResponse::Failed(error),
        },
        AccountRequest::CancelLogin { generation } => match cancel_login(service, generation) {
            Ok(view) => AccountResponse::Cancelled(view),
            Err(error) => AccountResponse::Failed(error),
        },
        AccountRequest::RefreshStatus { seq, epoch } => match fetch_account_status(service) {
            Ok(view) => AccountResponse::Status { view, seq, epoch },
            Err(error) => AccountResponse::StatusFailed { seq, epoch, error },
        },
        AccountRequest::RefreshProfile { epoch } => AccountResponse::ProfileRefreshQueued {
            view: service.request_profile_refresh(),
            epoch,
        },
        AccountRequest::SignOut => match sign_out(service) {
            Ok(view) => AccountResponse::SignedOut(view),
            Err(error) => AccountResponse::Failed(error),
        },
    }
}

fn account_service() -> &'static AccountService {
    static SERVICE: OnceLock<AccountService> = OnceLock::new();
    SERVICE.get_or_init(|| AccountService::new_restoring(AccountServiceConfig::from_environment()))
}

/// Signs out the shared engine-owned session.
///
/// # Errors
///
/// Returns an error when the request fails or the reply is invalid.
pub fn sign_out(service: &AccountService) -> Result<AccountView, String> {
    Ok(service.sign_out())
}

#[cfg(test)]
mod tests {
    use super::{
        AccountRequest, AccountResponse, DesktopAccount, MANAGE_PROFILE_URL, account_action_label,
        account_state_label, sanitized_plan_label, unavailable_menu_state,
    };
    use axiusflow_contracts::{AccountSessionState, AccountView};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Scripted fake runtime behind an isolated desktop session. The worker
    /// thread drives it exactly like production: Begin opens the browser
    /// transaction, status polls serve the current fake view, and the test
    /// flips the view to simulate callback arrival, completion, or expiry.
    /// Handler for direct-apply tests: the worker never changes state on
    /// its own, so scripted replies arrive in test order.
    fn inert_engine(request: AccountRequest) -> AccountResponse {
        match request {
            AccountRequest::RefreshStatus { seq, epoch } => AccountResponse::StatusFailed {
                seq,
                epoch,
                error: "inert test engine".to_string(),
            },
            _ => AccountResponse::Failed("inert test engine".to_string()),
        }
    }

    #[derive(Default)]
    struct FakeEngine {
        view: AccountView,
        begins: usize,
        begin_fails: bool,
        fail_status: bool,
        signed_out: AccountView,
    }

    impl FakeEngine {
        fn authorizing() -> AccountView {
            let mut view = view(AccountSessionState::Authorizing);
            view.detail = "waiting for browser authorization".to_string();
            view
        }

        fn handle(&mut self, request: AccountRequest) -> AccountResponse {
            match request {
                AccountRequest::BeginLogin { generation } => {
                    self.begins += 1;
                    if self.begin_fails {
                        return AccountResponse::Failed("fake engine is unreachable".to_string());
                    }
                    self.view = Self::authorizing();
                    self.view.request_generation = generation;
                    AccountResponse::Authorized(super::LoginAuthorization {
                        request_generation: generation,
                        authorization_url: format!(
                            "https://auth.axiusflow.com/authorize?request={generation}"
                        ),
                        expires_unix_seconds: 1_800_000_003,
                    })
                }
                AccountRequest::RefreshStatus { seq, epoch } => {
                    if self.fail_status {
                        return AccountResponse::StatusFailed {
                            seq,
                            epoch,
                            error: "fake status fetch failed".to_string(),
                        };
                    }
                    AccountResponse::Status {
                        view: self.view.clone(),
                        seq,
                        epoch,
                    }
                }
                AccountRequest::RefreshProfile { epoch } => AccountResponse::ProfileRefreshQueued {
                    view: self.view.clone(),
                    epoch,
                },
                AccountRequest::CancelLogin { .. } => {
                    self.view = self.signed_out.clone();
                    AccountResponse::Cancelled(self.view.clone())
                }
                AccountRequest::SignOut => {
                    self.view = self.signed_out.clone();
                    AccountResponse::SignedOut(self.view.clone())
                }
            }
        }

        fn complete_active(&mut self, name: &str, email: &str) {
            let mut view = view(AccountSessionState::Active);
            view.account_id = "acct_01".to_string();
            view.plan_id = "pro".to_string();
            view.display_name = name.to_string();
            view.email = email.to_string();
            view.photo_url = "https://auth.axiusflow.com/photo/ada.png".to_string();
            self.view = view;
        }
    }

    /// Spawns one isolated session backed by a scripted fake runtime,
    /// returning the session plus the engine state for the test to drive.
    fn scripted_session() -> (DesktopAccount, Arc<Mutex<FakeEngine>>) {
        let engine = Arc::new(Mutex::new(FakeEngine {
            signed_out: view(AccountSessionState::SignedOut),
            ..FakeEngine::default()
        }));
        engine.lock().expect("engine locks").view = view(AccountSessionState::SignedOut);
        let worker = Arc::clone(&engine);
        let session = DesktopAccount::spawn_with(move |request| {
            worker.lock().expect("engine locks").handle(request)
        })
        .expect("isolated account session spawns");
        (session, engine)
    }

    /// Polls until the condition holds or the test times out. The worker
    /// round-trips in milliseconds; the timeout only fires on real stalls.
    fn wait_for(session: &DesktopAccount, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = session.poll();
            if done() {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Rewinds the status clock so the next poll fetches immediately
    /// instead of waiting out the authorizing interval.
    fn rewind_status(session: &DesktopAccount) {
        use super::STATUS_POLL_INTERVAL;

        *session
            .shared
            .last_status_poll
            .lock()
            .expect("status clock locks") = Instant::now()
            .checked_sub(STATUS_POLL_INTERVAL + Duration::from_secs(1))
            .expect("test clock rewinds");
    }

    fn view(state: AccountSessionState) -> AccountView {
        AccountView {
            state,
            account_id: String::new(),
            plan_id: String::new(),
            detail: String::new(),
            request_generation: 1,
            display_name: String::new(),
            email: String::new(),
            photo_url: String::new(),
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
            "Sign-in expired"
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
    fn signed_out_hides_identity_but_errors_and_recovery_show() {
        use super::{AccountMenuState, AccountPresentation};

        let presentation = |action: &'static str, state: &'static str| AccountPresentation {
            action,
            state,
            plan: "No plan",
            detail: String::new(),
            display_name: String::new(),
            email: String::new(),
            photo_url: String::new(),
            pending: false,
        };
        // Plain signed out: only the Sign in row (plus any error) renders.
        let signed_out = AccountMenuState {
            presentation: presentation("Sign in", "Signed out"),
            error: None,
        };
        assert!(signed_out.hides_identity());
        assert!(!signed_out.signed_in());
        assert!(!signed_out.authorizing());
        // Signed out with an error still hides identity metadata; the error
        // row carries the actionable message.
        let failed = AccountMenuState {
            presentation: presentation("Sign in", "Signed out"),
            error: Some("account request timed out; try again".to_string()),
        };
        assert!(failed.hides_identity());
        // Every other state names itself: unavailable engine, browser wait,
        // recovery states, and authenticated sessions all show the header.
        for (action, state) in [
            ("Sign in", "Sign-in unavailable"),
            ("Waiting for browser", "Waiting for browser sign-in"),
            ("Sign in", "Sign-in required"),
            ("Sign in", "Subscription expired"),
            ("Sign in", "Sign-in unavailable"),
            ("Account", "Signed in"),
            ("Account", "Signed in (offline)"),
        ] {
            let menu = AccountMenuState {
                presentation: presentation(action, state),
                error: None,
            };
            assert!(!menu.hides_identity(), "state must stay visible: {state}");
        }
    }

    #[test]
    fn initials_fall_back_from_name_to_email_to_placeholder() {
        use super::profile_initials;

        assert_eq!(profile_initials("Ada Trader", "ada@example.com"), "AT");
        assert_eq!(profile_initials("Ada", "ada@example.com"), "A");
        assert_eq!(profile_initials("  ada   trader  ", "x@y.z"), "AT");
        assert_eq!(profile_initials("", "ada@example.com"), "AD");
        assert_eq!(profile_initials("", "b.o.b@example.com"), "BO");
        assert_eq!(profile_initials("", ""), "A");
        assert_eq!(profile_initials("123 456", ""), "14");
        // Initials never come back empty: the avatar always has content
        // when the photo is absent or fails to load.
        for (name, email) in [("", ""), (" - ", ""), ("", "@")] {
            assert!(!profile_initials(name, email).is_empty());
        }
    }

    #[test]
    fn only_https_photos_reach_the_image_loader() {
        use super::has_profile_photo;

        assert!(has_profile_photo("https://auth.axiusflow.com/photo/a.png"));
        assert!(has_profile_photo(
            "https://lh3.googleusercontent.com/a/photo?x=1&y=2"
        ));
        for bad in [
            "",
            "http://auth.axiusflow.com/photo/a.png",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://auth.axiusflow.com/has space",
            "data:image/png;base64,AAA",
        ] {
            assert!(!has_profile_photo(bad), "photo must not load: {bad}");
        }
        assert!(!has_profile_photo(&format!(
            "https://auth.axiusflow.com/{}",
            "a".repeat(2048)
        )));
    }

    #[test]
    fn manage_profile_targets_the_production_account_hub() {
        assert_eq!(
            MANAGE_PROFILE_URL,
            "https://auth.axiusflow.com/account?section=profile"
        );
    }

    #[test]
    fn status_profile_propagates_to_presentation_and_clears() {
        use super::{AccountResponse, apply_account_response};
        use std::sync::atomic::Ordering;

        // The startup fetch owns seq 1/epoch 0; the inert worker only ever
        // reports failure, so scripted replies arrive in test order.
        let session = DesktopAccount::spawn_with(inert_engine).expect("isolated session spawns");
        assert_eq!(session.shared.status_seq.load(Ordering::Acquire), 1);
        let mut active = view(AccountSessionState::Active);
        active.account_id = "acct_01".to_string();
        active.plan_id = "pro".to_string();
        active.display_name = "Ada Trader".to_string();
        active.email = "ada@example.com".to_string();
        active.photo_url = "https://auth.axiusflow.com/photo/ada.png".to_string();
        apply_account_response(
            &session.shared,
            AccountResponse::Status {
                view: active,
                seq: 1,
                epoch: 0,
            },
        );
        let presentation = session.presentation();
        assert_eq!(presentation.action, "Account");
        assert_eq!(presentation.display_name, "Ada Trader");
        assert_eq!(presentation.email, "ada@example.com");
        assert_eq!(
            presentation.photo_url,
            "https://auth.axiusflow.com/photo/ada.png"
        );
        // Sign-out status wipes the profile: no stale identity survives.
        apply_account_response(
            &session.shared,
            AccountResponse::SignedOut(view(AccountSessionState::SignedOut)),
        );
        let signed_out = session.presentation();
        assert!(signed_out.display_name.is_empty());
        assert!(signed_out.email.is_empty());
        assert!(signed_out.photo_url.is_empty());
    }

    #[test]
    fn installed_session_starts_signed_out() {
        // Install uses the production worker, so only synchronous local
        // state is asserted here: worker replies race the test thread and
        // are covered by the scripted sessions below.
        let session = DesktopAccount::install().expect("account session installs");
        let presentation = session.presentation();
        assert_eq!(presentation.action, "Sign in");
        assert!(!presentation.pending);
        let menu = session.menu_state();
        assert_eq!(menu.presentation.action, "Sign in");
    }

    #[test]
    fn failed_startup_status_remains_in_verification_state() {
        let session = DesktopAccount::spawn_with(inert_engine).expect("isolated session spawns");
        wait_for(&session, "startup status failure", || {
            session.error().is_some()
        });
        assert!(session.verification_pending());
    }

    #[test]
    fn startup_restore_stays_in_verification_until_engine_resolves_it() {
        use super::{AccountResponse, apply_account_response};
        let session = DesktopAccount::spawn_with(inert_engine).expect("session");
        let mut restoring = view(AccountSessionState::Authorizing);
        restoring.request_generation = 0;
        apply_account_response(
            &session.shared,
            AccountResponse::Status {
                view: restoring,
                seq: 1,
                epoch: 0,
            },
        );
        assert!(session.verification_pending());
        assert!(!session.authenticated());
        apply_account_response(
            &session.shared,
            AccountResponse::Status {
                view: view(AccountSessionState::Active),
                seq: 1,
                epoch: 0,
            },
        );
        assert!(!session.verification_pending());
        assert!(session.authenticated());
        apply_account_response(
            &session.shared,
            AccountResponse::Status {
                view: view(AccountSessionState::Authorizing),
                seq: 1,
                epoch: 0,
            },
        );
        assert!(
            !session.verification_pending(),
            "interactive login has its own presentation"
        );
    }

    #[test]
    fn generations_seed_from_the_wall_clock_for_runtime_fencing() {
        use super::unix_millis;
        use std::sync::atomic::Ordering;

        // A fresh process must supersede generations from a previous process
        // lifetime: persisted account material outlives desktop restarts, and a
        // counter restarted at zero would read as retired.
        assert!(unix_millis() > 0);
        let session = DesktopAccount::spawn_with(inert_engine).expect("isolated session spawns");
        assert!(session.shared.generation.load(Ordering::Acquire) > 0);
    }

    #[test]
    fn browser_reopen_uses_only_the_retained_authorization_url() {
        use super::{AccountResponse, LoginAuthorization, apply_account_response};

        let session = DesktopAccount::spawn_with(inert_engine).expect("isolated session spawns");
        // No transaction yet: nothing to reopen.
        assert!(session.reopen_browser().is_err());
        let authorization = LoginAuthorization {
            request_generation: 3,
            authorization_url: "https://auth.axiusflow.com/authorize?request=3".to_string(),
            expires_unix_seconds: 1_800_000_003,
        };
        apply_account_response(&session.shared, AccountResponse::Authorized(authorization));
        let stored = session
            .shared
            .authorization_url
            .lock()
            .expect("url slot locks")
            .clone();
        assert_eq!(
            stored.as_deref(),
            Some("https://auth.axiusflow.com/authorize?request=3")
        );
    }

    #[test]
    fn full_sign_in_sequence_reaches_verified_profile() {
        use std::sync::atomic::Ordering;

        // Click, browser confirmation, callback, engine completion, profile
        // update: the exact production order through the worker thread.
        let (session, engine) = scripted_session();
        session.request_sign_in().expect("sign-in queues");
        // The transaction opens and polling starts at once: pending clears
        // while the browser holds the transaction, so Reopen and Cancel
        // stay enabled instead of sticking disabled.
        wait_for(&session, "transaction open", || {
            session.shared.login_open.load(Ordering::Acquire)
        });
        assert!(!session.shared.pending.load(Ordering::Acquire));
        assert!(
            session
                .shared
                .authorization_url
                .lock()
                .expect("url slot locks")
                .is_some()
        );
        wait_for(&session, "authorizing view", || {
            session.presentation().action == "Waiting for browser"
        });
        // The browser callback completes inside the account runtime.
        engine
            .lock()
            .expect("engine locks")
            .complete_active("Ada Trader", "ada@example.com");
        rewind_status(&session);
        wait_for(&session, "active profile", || {
            session.presentation().action == "Account"
        });
        let presentation = session.presentation();
        assert_eq!(presentation.display_name, "Ada Trader");
        assert_eq!(presentation.email, "ada@example.com");
        assert!(!presentation.photo_url.is_empty());
        assert!(session.error().is_none());
        // Terminal state closes the transaction and drops the URL.
        assert!(!session.shared.login_open.load(Ordering::Acquire));
        assert!(
            session
                .shared
                .authorization_url
                .lock()
                .expect("url slot locks")
                .is_none()
        );
    }

    #[test]
    fn immediate_callback_completion_wins_cleanly() {
        use std::sync::atomic::Ordering;

        // The callback lands before the first status fetch: completion must
        // still resolve instead of stranding pending state.
        let (session, engine) = scripted_session();
        session.request_sign_in().expect("sign-in queues");
        wait_for(&session, "transaction open", || {
            session.shared.login_open.load(Ordering::Acquire)
        });
        engine
            .lock()
            .expect("engine locks")
            .complete_active("Ada Trader", "ada@example.com");
        rewind_status(&session);
        wait_for(&session, "active profile", || {
            session.presentation().action == "Account"
        });
        assert!(!session.shared.pending.load(Ordering::Acquire));
        assert!(session.error().is_none());
        assert_eq!(session.presentation().display_name, "Ada Trader");
    }

    #[test]
    fn cancel_during_authorizing_resolves_signed_out() {
        use std::sync::atomic::Ordering;

        let (session, _) = scripted_session();
        session.request_sign_in().expect("sign-in queues");
        wait_for(&session, "authorizing view", || {
            session.presentation().action == "Waiting for browser"
        });
        session.request_cancel().expect("cancel queues");
        wait_for(&session, "cancelled", || {
            !session.shared.login_open.load(Ordering::Acquire)
                && session.presentation().action == "Sign in"
        });
        assert!(!session.shared.pending.load(Ordering::Acquire));
        assert!(session.error().is_none());
    }

    #[test]
    fn repeated_sign_in_clicks_start_one_transaction() {
        use std::sync::atomic::Ordering;

        let (session, engine) = scripted_session();
        session.request_sign_in().expect("first click queues");
        session.request_sign_in().expect("second click collapses");
        session.request_sign_in().expect("third click collapses");
        wait_for(&session, "transaction open", || {
            session.shared.login_open.load(Ordering::Acquire)
        });
        // One BeginLogin reached the engine despite three clicks; the
        // follow-ups collapsed into the in-flight request.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(engine.lock().expect("engine locks").begins, 1);
    }

    #[test]
    fn dead_engine_reports_actionable_error_with_retry() {
        use std::sync::atomic::Ordering;

        let (session, engine) = scripted_session();
        engine.lock().expect("engine locks").begin_fails = true;
        session.request_sign_in().expect("sign-in queues");
        // The failure resolves to an error with retry enabled, never a
        // stuck disabled button.
        wait_for(&session, "failure surfaces", || session.error().is_some());
        assert!(!session.shared.pending.load(Ordering::Acquire));
        assert!(!session.shared.login_open.load(Ordering::Acquire));
        // Recovery: the retry starts a fresh generation.
        engine.lock().expect("engine locks").begin_fails = false;
        session.request_sign_in().expect("retry queues");
        wait_for(&session, "retry opens", || {
            session.shared.login_open.load(Ordering::Acquire)
        });
        assert_eq!(engine.lock().expect("engine locks").begins, 2);
        assert!(session.error().is_none());
    }

    #[test]
    fn stale_status_replies_cannot_overwrite_current_state() {
        use super::apply_account_response;
        use std::sync::atomic::Ordering;

        let (session, _) = scripted_session();
        session.request_sign_in().expect("sign-in queues");
        wait_for(&session, "authorizing view", || {
            session.presentation().action == "Waiting for browser"
        });
        // A superseded sequence number drops without touching the slot.
        let bogus = view(AccountSessionState::Active);
        apply_account_response(
            &session.shared,
            AccountResponse::Status {
                view: bogus,
                seq: 999_999,
                epoch: 0,
            },
        );
        assert_eq!(session.presentation().action, "Waiting for browser");
        // A current-sequence reply from a retired epoch (cancelled,
        // retried, or signed out since) drops without touching state.
        let seq = session.shared.status_seq.load(Ordering::Acquire);
        let mut active = view(AccountSessionState::Active);
        active.display_name = "Mallory".to_string();
        apply_account_response(
            &session.shared,
            AccountResponse::Status {
                view: active,
                seq,
                epoch: 0,
            },
        );
        assert_eq!(session.presentation().action, "Waiting for browser");
        assert!(session.presentation().display_name.is_empty());
    }

    #[test]
    fn status_failure_keeps_polling_and_clears_on_recovery() {
        let (session, engine) = scripted_session();
        session.request_sign_in().expect("sign-in queues");
        wait_for(&session, "authorizing view", || {
            session.presentation().action == "Waiting for browser"
        });
        // Failing fetches surface an actionable error while the
        // transaction stays open: polling is the automatic retry and
        // Reopen/Cancel stay enabled throughout.
        engine.lock().expect("engine locks").fail_status = true;
        rewind_status(&session);
        wait_for(&session, "fetch error surfaces", || {
            session.error().is_some()
        });
        assert!(
            session
                .shared
                .login_open
                .load(std::sync::atomic::Ordering::Acquire)
        );
        engine.lock().expect("engine locks").fail_status = false;
        rewind_status(&session);
        wait_for(&session, "error clears", || session.error().is_none());
        assert_eq!(session.presentation().action, "Waiting for browser");
    }

    #[test]
    fn sign_out_clears_profile_and_allows_account_switch() {
        let (session, engine) = scripted_session();
        session.request_sign_in().expect("sign-in queues");
        wait_for(&session, "authorizing view", || {
            session.presentation().action == "Waiting for browser"
        });
        engine
            .lock()
            .expect("engine locks")
            .complete_active("Ada Trader", "ada@example.com");
        rewind_status(&session);
        wait_for(&session, "active profile", || {
            session.presentation().action == "Account"
        });
        session.request_sign_out().expect("sign-out queues");
        wait_for(&session, "signed out", || {
            session.presentation().action == "Sign in"
        });
        let cleared = session.presentation();
        assert!(cleared.display_name.is_empty());
        assert!(cleared.email.is_empty());
        assert!(cleared.photo_url.is_empty());
        // A second user signs in on a fresh generation with no carryover.
        session.request_sign_in().expect("second sign-in queues");
        wait_for(&session, "authorizing again", || {
            session.presentation().action == "Waiting for browser"
        });
        engine
            .lock()
            .expect("engine locks")
            .complete_active("Bobgnome", "bob@example.com");
        rewind_status(&session);
        wait_for(&session, "switched profile", || {
            session.presentation().display_name == "Bobgnome"
        });
        assert_eq!(session.presentation().email, "bob@example.com");
    }

    #[test]
    fn restart_restores_active_session_from_first_status() {
        // A fresh desktop presentation against an already signed-in account runtime
        // learns the session from its startup fetch: no click needed.
        let engine = Arc::new(Mutex::new(FakeEngine {
            signed_out: view(AccountSessionState::SignedOut),
            ..FakeEngine::default()
        }));
        engine
            .lock()
            .expect("engine locks")
            .complete_active("Ada Trader", "ada@example.com");
        let worker = Arc::clone(&engine);
        let session = DesktopAccount::spawn_with(move |request| {
            worker.lock().expect("engine locks").handle(request)
        })
        .expect("isolated session spawns");
        wait_for(&session, "restored session", || {
            session.presentation().action == "Account"
        });
        assert!(!session.verification_pending());
        assert_eq!(session.presentation().display_name, "Ada Trader");
    }

    #[test]
    fn verified_cached_offline_lease_keeps_the_platform_open() {
        let (session, engine) = scripted_session();
        let mut offline = view(AccountSessionState::OfflineLease);
        offline.account_id = "acct_01".to_string();
        offline.plan_id = "pro".to_string();
        engine.lock().expect("engine locks").view = offline;
        rewind_status(&session);
        wait_for(&session, "offline lease", || {
            session.presentation().state == account_state_label(AccountSessionState::OfflineLease)
        });

        assert!(session.authenticated());
    }

    #[test]
    fn stuck_requests_time_out_instead_of_blocking_forever() {
        use super::REQUEST_TIMEOUT;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let session = DesktopAccount::spawn_with(inert_engine).expect("isolated session spawns");
        session.shared.pending.store(true, Ordering::Release);
        *session
            .shared
            .request_at
            .lock()
            .expect("request clock locks") = Instant::now()
            .checked_sub(REQUEST_TIMEOUT + Duration::from_secs(1))
            .expect("test clock rewinds");
        assert!(session.poll());
        assert!(!session.shared.pending.load(Ordering::Acquire));
        assert_eq!(
            session.error().as_deref(),
            Some("account request timed out; try again")
        );
    }
}
