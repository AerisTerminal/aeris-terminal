//! Runtime-owned native account session.
//!
//! One [`AccountService`] is shared by all desktop windows. It owns the PKCE
//! transaction, the loopback listener, the OIDC exchange with the Aeris
//! Cognito user pool, vault material, and the generation-fenced session
//! state. It owns no market-coordinator, provider, or chart state, and
//! account refresh never recreates a provider session.
//!
//! Every signed-in account has the same unlimited early-access plan, so the
//! session carries identity and profile only; there is no entitlement lease.

pub mod loopback;
pub mod oidc;
pub mod pkce;

use aeris_observability::diagnostic;
use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use aeris_account::{
    AccountId, MAXIMUM_PROFILE_EMAIL_BYTES, MAXIMUM_PROFILE_NAME_BYTES,
    MAXIMUM_PROFILE_PHOTO_URL_BYTES, PlanId,
};
use aeris_contracts::{AccountSessionState, AccountView, LoginAuthorization};
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use zeroize::Zeroizing;

use loopback::{CALLBACK_PORTS, LoopbackListener, validate_callback_query};
use oidc::{
    AccountProfile, AuthorizationRequest, OidcEndpoints, authorization_url, discover,
    exchange_code, oidc_agent, refresh_grant,
};
use pkce::{PkceVerifier, generate_oauth_random};

/// How long one login transaction waits for the browser callback.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// Public native app client registered with the Aeris user pool (no secret).
const NATIVE_CLIENT_ID: &str = "tlnlg2amke4k2v9v2k84vvg87";
/// Aeris Cognito user-pool issuer; discovery and JWKS are served here.
const AUTH_ISSUER: &str = "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_05pNG4EOF";
/// Hosted sign-in origin serving the authorize, token and revoke endpoints.
const HOSTED_SIGN_IN_ORIGIN: &str = "https://auth.aeristerminal.com";

const ACCOUNT_VAULT_SERVICE: &str = "com.aeris.account";
const REFRESH_VAULT_KEY: &str = "account-refresh-default-v1";
const PROFILE_VAULT_KEY: &str = "account-profile-v1";
const REJECTED_RESTORE_MARKER_FILE: &str = "account-restore-rejected-v1";
const PROFILE_VAULT_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_PROFILE_VAULT_BYTES: usize = 4096;

/// Interval between verified session refreshes while online.
const SESSION_REFRESH_INTERVAL: Duration = Duration::from_hours(1);
/// Upper bound of the random delay added to each scheduled refresh.
const SESSION_REFRESH_JITTER_SECONDS: u64 = 300;
/// First retry delay after the identity provider was unreachable.
const OFFLINE_RETRY_INITIAL: Duration = Duration::from_secs(15);
/// Longest retry delay while the identity provider stays unreachable.
const OFFLINE_RETRY_MAXIMUM: Duration = Duration::from_mins(10);
/// Retry delay while a process-lifecycle quiesce holds refresh grants.
const QUIESCED_RETRY: Duration = Duration::from_secs(60);

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CachedAccountProfile {
    schema_version: u32,
    account_id: String,
    display_name: String,
    email: String,
    photo_url: String,
}

/// Local startup readiness of the production saved-session restore path.
///
/// This covers native-vault access and cached-session classification only.
/// Remote refresh work remains asynchronous and is intentionally not part of
/// candidate activation health.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AccountRestoreReadiness {
    /// Local saved-session state has not been classified yet.
    Pending = 0,
    /// Native storage was readable and local saved-session state was classified.
    Ready = 1,
    /// Native storage or local restore startup failed before that boundary.
    Failed = 2,
}

/// Identity-provider configuration for one account service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountServiceConfig {
    issuer: String,
    hosted_origin: String,
    client_id: String,
}

impl AccountServiceConfig {
    /// The provisioned Aeris user pool and native app client.
    #[must_use]
    pub fn production() -> Self {
        Self {
            issuer: AUTH_ISSUER.to_string(),
            hosted_origin: HOSTED_SIGN_IN_ORIGIN.to_string(),
            client_id: NATIVE_CLIENT_ID.to_string(),
        }
    }

    /// Creates a validated configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the issuer or hosted origin is not an `https`
    /// address or the client identifier is empty.
    pub fn try_new(
        issuer: impl Into<String>,
        hosted_origin: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Result<Self, String> {
        let config = Self {
            issuer: issuer.into(),
            hosted_origin: hosted_origin.into(),
            client_id: client_id.into(),
        };
        if oidc::parse_https_uri(&config.issuer).is_err()
            || oidc::parse_https_uri(&config.hosted_origin).is_err()
            || config.client_id.trim().is_empty()
        {
            return Err("account service configuration is invalid".to_string());
        }
        Ok(config)
    }
}

struct PendingLogin {
    generation: u64,
    oauth_state: String,
    nonce: String,
    verifier: PkceVerifier,
    redirect_uri: String,
    expires_at: Instant,
    code_received: bool,
}

struct ServiceState {
    view: AccountView,
    pending: Option<PendingLogin>,
    last_generation: u64,
    restore_allowed: bool,
}

#[derive(Clone)]
enum RejectedRestoreMarker {
    #[cfg(test)]
    Disabled,
    Unavailable,
    Path(PathBuf),
}

/// Runtime-owned account session shared by all desktop windows.
#[derive(Clone)]
pub struct AccountService {
    config: AccountServiceConfig,
    callback_ports: &'static [u16],
    rejected_restore_marker: RejectedRestoreMarker,
    state: Arc<Mutex<ServiceState>>,
    endpoints: Arc<Mutex<Option<OidcEndpoints>>>,
    refresh_gate: Arc<Mutex<()>>,
    refresh_lifecycle: Arc<Mutex<RefreshLifecycleState>>,
    profile_refresh_in_flight: Arc<AtomicBool>,
    restore_started: Arc<AtomicBool>,
    restore_readiness: Arc<AtomicU8>,
}

#[derive(Default)]
struct RefreshLifecycleState {
    quiesce_claims: usize,
    grants_in_flight: usize,
}

/// One process-lifecycle claim that prevents new refresh grants and can wait
/// for any already-running refresh round to finish its durable token commit.
///
/// Dropping a claim resumes refreshes when no other quiesce owner remains.
/// Successful process shutdown can retain a claim permanently because the
/// process exits immediately afterward.
pub struct AccountRefreshQuiesce {
    refresh_lifecycle: Arc<Mutex<RefreshLifecycleState>>,
    release_on_drop: bool,
}

impl AccountRefreshQuiesce {
    const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

    /// Waits for the refresh owner to become idle after this quiesce claim was
    /// installed. The bound exceeds the account HTTP agent's global timeout so
    /// an in-flight grant can reach its mandatory rotated-token vault commit.
    ///
    /// # Errors
    ///
    /// Returns an actionable redacted error when the refresh owner does not
    /// drain within the bounded shutdown interval.
    pub fn wait(&self) -> Result<(), String> {
        self.wait_for(Self::WAIT_TIMEOUT)
    }

    fn wait_for(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let grants_in_flight = self
                .refresh_lifecycle
                .lock()
                .map_err(|_| "account refresh lifecycle state is unavailable".to_string())?
                .grants_in_flight;
            if grants_in_flight == 0 {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(
                    "account refresh did not finish durable credential rotation before shutdown"
                        .to_string(),
                );
            }
            std::thread::sleep(Duration::from_millis(10).min(deadline - now));
        }
    }

    /// Keeps this quiesce claim installed until process exit instead of
    /// resuming refresh work when the waiter is dropped.
    pub fn retain_until_process_exit(mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for AccountRefreshQuiesce {
    fn drop(&mut self) {
        if !self.release_on_drop {
            return;
        }
        let mut lifecycle = self
            .refresh_lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifecycle.quiesce_claims = lifecycle.quiesce_claims.saturating_sub(1);
    }
}

struct RefreshGrantPermit {
    refresh_lifecycle: Arc<Mutex<RefreshLifecycleState>>,
}

impl Drop for RefreshGrantPermit {
    fn drop(&mut self) {
        let mut lifecycle = self
            .refresh_lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifecycle.grants_in_flight = lifecycle.grants_in_flight.saturating_sub(1);
    }
}

enum LocalRestore {
    Settled,
    ContinueOnline,
}

/// Outcome of one refresh round against the identity provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionRefresh {
    /// The session verified online and its profile was refreshed.
    Verified,
    /// A process-lifecycle quiesce postponed the grant.
    Deferred,
    /// The identity provider was unreachable; the session stays signed in.
    Unavailable,
    /// The generation retired or the session was signed out or failed closed.
    Retired,
}

/// Bounded exponential retry delay while the identity provider is unreachable.
struct OfflineBackoff {
    next: Duration,
}

impl OfflineBackoff {
    const fn new() -> Self {
        Self {
            next: OFFLINE_RETRY_INITIAL,
        }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(OFFLINE_RETRY_MAXIMUM);
        delay
    }
}

impl AccountService {
    /// Creates an account session starting signed out.
    #[must_use]
    pub fn new(config: AccountServiceConfig) -> Self {
        let rejected_restore_marker = aeris_platform_runtime::native_data_root()
            .map_or(RejectedRestoreMarker::Unavailable, |root| {
                RejectedRestoreMarker::Path(root.join(REJECTED_RESTORE_MARKER_FILE))
            });
        Self::new_with_rejected_restore_marker(config, rejected_restore_marker)
    }

    fn new_with_rejected_restore_marker(
        config: AccountServiceConfig,
        rejected_restore_marker: RejectedRestoreMarker,
    ) -> Self {
        Self {
            config,
            callback_ports: &CALLBACK_PORTS,
            rejected_restore_marker,
            state: Arc::new(Mutex::new(ServiceState {
                view: cleared_view(AccountSessionState::SignedOut, 0, "signed out"),
                pending: None,
                last_generation: 0,
                restore_allowed: true,
            })),
            endpoints: Arc::new(Mutex::new(None)),
            refresh_gate: Arc::new(Mutex::new(())),
            refresh_lifecycle: Arc::new(Mutex::new(RefreshLifecycleState::default())),
            profile_refresh_in_flight: Arc::new(AtomicBool::new(false)),
            restore_started: Arc::new(AtomicBool::new(false)),
            restore_readiness: Arc::new(AtomicU8::new(AccountRestoreReadiness::Ready as u8)),
        }
    }

    fn rejected_restore_marker_present(&self) -> Result<bool, ()> {
        let path = match &self.rejected_restore_marker {
            #[cfg(test)]
            RejectedRestoreMarker::Disabled => return Ok(false),
            RejectedRestoreMarker::Unavailable => return Err(()),
            RejectedRestoreMarker::Path(path) => path,
        };
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Ok(_) | Err(_) => Err(()),
        }
    }

    fn persist_rejected_restore_marker(&self) -> Result<(), ()> {
        let path = match &self.rejected_restore_marker {
            #[cfg(test)]
            RejectedRestoreMarker::Disabled => return Err(()),
            RejectedRestoreMarker::Unavailable => return Err(()),
            RejectedRestoreMarker::Path(path) => path,
        };
        let parent = path.parent().ok_or(())?;
        fs::create_dir_all(parent).map_err(|_| ())?;
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                return Ok(());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) | Err(_) => return Err(()),
        }
        let file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return self
                    .rejected_restore_marker_present()
                    .and_then(|present| present.then_some(()).ok_or(()));
            }
            Err(_) => return Err(()),
        };
        file.sync_all().map_err(|_| ())?;
        #[cfg(unix)]
        sync_account_marker_directory(parent)?;
        Ok(())
    }

    fn clear_rejected_restore_marker(&self) -> Result<(), ()> {
        let path = match &self.rejected_restore_marker {
            #[cfg(test)]
            RejectedRestoreMarker::Disabled => return Ok(()),
            RejectedRestoreMarker::Unavailable => return Err(()),
            RejectedRestoreMarker::Path(path) => path,
        };
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Ok(_) | Err(_) => return Err(()),
        }
        fs::remove_file(path).map_err(|_| ())?;
        #[cfg(unix)]
        sync_account_marker_directory(path.parent().ok_or(())?)?;
        Ok(())
    }

    /// Starts production saved-session restoration once for this shared
    /// service. This can be called after process-lifecycle guards are installed
    /// so no refresh grant exists before native shutdown fencing is ready.
    ///
    /// A saved session with a cached verified profile signs in immediately as
    /// [`AccountSessionState::Offline`] while the refresh grant verifies it.
    pub fn start_restore(&self) {
        if self.restore_started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.set_restore_readiness(AccountRestoreReadiness::Pending);
        if let Ok(mut state) = self.state.lock() {
            state.view = cleared_view(
                AccountSessionState::Authorizing,
                0,
                "restoring saved sign-in",
            );
        }
        let restoring = self.clone();
        if std::thread::Builder::new()
            .name("aeris-account-restore".to_string())
            .spawn(move || restoring.restore_online_session())
            .is_err()
        {
            self.set_restore_readiness(AccountRestoreReadiness::Failed);
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "saved sign-in restore could not start; sign in again",
            );
        }
    }

    /// Returns the bounded local readiness of production saved-session restore.
    #[must_use]
    pub fn restore_readiness(&self) -> AccountRestoreReadiness {
        match self.restore_readiness.load(Ordering::Acquire) {
            0 => AccountRestoreReadiness::Pending,
            1 => AccountRestoreReadiness::Ready,
            _ => AccountRestoreReadiness::Failed,
        }
    }

    fn set_restore_readiness(&self, readiness: AccountRestoreReadiness) {
        self.restore_readiness
            .store(readiness as u8, Ordering::Release);
    }

    /// Prevents new refresh grants from starting and returns a bounded waiter
    /// for any grant that already reached the identity provider. The waiter
    /// covers the grant through its mandatory rotated-refresh-token vault commit.
    ///
    /// # Errors
    ///
    /// Returns a redacted error only if the bounded quiesce-claim counter is
    /// exhausted.
    pub fn begin_refresh_quiesce(&self) -> Result<AccountRefreshQuiesce, String> {
        let mut lifecycle = self
            .refresh_lifecycle
            .lock()
            .map_err(|_| "account refresh lifecycle state is unavailable".to_string())?;
        lifecycle.quiesce_claims = lifecycle
            .quiesce_claims
            .checked_add(1)
            .ok_or_else(|| "account refresh shutdown capacity is exhausted".to_string())?;
        drop(lifecycle);
        Ok(AccountRefreshQuiesce {
            refresh_lifecycle: Arc::clone(&self.refresh_lifecycle),
            release_on_drop: true,
        })
    }

    fn claim_refresh_grant(&self) -> Result<Option<RefreshGrantPermit>, String> {
        let mut lifecycle = self
            .refresh_lifecycle
            .lock()
            .map_err(|_| "account refresh lifecycle state is unavailable".to_string())?;
        if lifecycle.quiesce_claims != 0 {
            return Ok(None);
        }
        lifecycle.grants_in_flight = lifecycle
            .grants_in_flight
            .checked_add(1)
            .ok_or_else(|| "account refresh grant capacity is exhausted".to_string())?;
        drop(lifecycle);
        Ok(Some(RefreshGrantPermit {
            refresh_lifecycle: Arc::clone(&self.refresh_lifecycle),
        }))
    }

    fn wait_for_refresh_resume(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let Ok(lifecycle) = self.refresh_lifecycle.lock() else {
                return false;
            };
            if lifecycle.quiesce_claims == 0 {
                return true;
            }
            drop(lifecycle);
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10).min(deadline - now));
        }
    }

    fn restore_online_session(&self) {
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            self.set_restore_readiness(AccountRestoreReadiness::Failed);
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "credential storage is unavailable; sign in again",
            );
            return;
        };
        let LocalRestore::ContinueOnline = self.restore_local_session(&vault) else {
            return;
        };
        // Candidate readiness ends at the local-vault boundary above. Everything
        // below performs bounded remote work and must not make activation
        // depend on current identity-provider reachability.
        let agent = oidc_agent();
        let endpoints = self.oidc_endpoints();
        let tokens = loop {
            let Ok(refresh_guard) = self.refresh_gate.lock() else {
                break None;
            };
            let permit = match self.claim_refresh_grant() {
                Ok(Some(permit)) => permit,
                Ok(None) => {
                    drop(refresh_guard);
                    if self.wait_for_refresh_resume(AccountRefreshQuiesce::WAIT_TIMEOUT) {
                        continue;
                    }
                    self.accept_online_restore_refresh(
                        &vault,
                        Err(oidc::RefreshGrantError::Unavailable),
                    );
                    break None;
                }
                Err(_) => break None,
            };
            let refresh_token = match self.current_restore_refresh_token(&vault) {
                Ok(Some(refresh_token)) => refresh_token,
                Ok(None) => break None,
                Err(()) => {
                    self.complete_restore_without_session(
                        AccountSessionState::TerminalError,
                        "credential storage is unavailable; sign in again",
                    );
                    break None;
                }
            };
            let outcome = endpoints
                .as_ref()
                .map_err(|_| oidc::RefreshGrantError::Unavailable)
                .and_then(|endpoints| {
                    refresh_grant(endpoints, &agent, &self.config.client_id, &refresh_token)
                });
            // Serialize startup restoration with scheduled/profile refreshes,
            // and keep the lifecycle permit through the mandatory rotated-token
            // vault commit. A cancelled update quiesce lets this
            // generation-zero restore resume instead of abandoning it.
            let tokens = self.accept_online_restore_refresh(&vault, outcome);
            drop(permit);
            break tokens;
        };
        if let Some(tokens) = tokens
            && self.apply_online_restore_tokens(&vault, &tokens)
        {
            self.spawn_session_worker(0, SESSION_REFRESH_INTERVAL);
        } else if self.hand_off_offline_restore() {
            self.spawn_session_worker(0, OFFLINE_RETRY_INITIAL);
        }
    }

    fn restore_local_session<V>(&self, vault: &V) -> LocalRestore
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        if !matches!(self.rejected_restore_marker_present(), Ok(false)) {
            self.set_restore_readiness(AccountRestoreReadiness::Failed);
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "saved sign-in could not be retired; credential storage is unavailable; sign in again",
            );
            return LocalRestore::Settled;
        }
        let refresh_token = match vault.load(REFRESH_VAULT_KEY) {
            Ok(Some(bytes)) => String::from_utf8(bytes)
                .ok()
                .filter(|token| !token.is_empty())
                .map(Zeroizing::new),
            Ok(None) => {
                if self
                    .complete_restore_without_session(AccountSessionState::SignedOut, "signed out")
                {
                    self.set_restore_readiness(AccountRestoreReadiness::Ready);
                } else {
                    self.set_restore_readiness(AccountRestoreReadiness::Failed);
                }
                return LocalRestore::Settled;
            }
            Err(_) => {
                self.set_restore_readiness(AccountRestoreReadiness::Failed);
                self.complete_restore_without_session(
                    AccountSessionState::TerminalError,
                    "credential storage is unavailable; sign in again",
                );
                return LocalRestore::Settled;
            }
        };
        if refresh_token.is_none() {
            if self.complete_restore_without_session(
                AccountSessionState::ReauthenticationRequired,
                "saved sign-in expired; sign in again",
            ) {
                self.set_restore_readiness(AccountRestoreReadiness::Ready);
            } else {
                self.set_restore_readiness(AccountRestoreReadiness::Failed);
            }
            return LocalRestore::Settled;
        }
        match load_profile_material(vault) {
            Ok(Some((account_id, profile))) => self.install_cached_session(&account_id, profile),
            Ok(None) => {}
            // Candidate activation fails closed when the cached profile is
            // unreadable; a readable refresh token still restores online.
            Err(()) => {
                self.set_restore_readiness(AccountRestoreReadiness::Failed);
                return LocalRestore::ContinueOnline;
            }
        }
        self.set_restore_readiness(AccountRestoreReadiness::Ready);
        LocalRestore::ContinueOnline
    }

    /// Signs the saved session in from its cached verified profile before the
    /// refresh grant confirms it, so a restart reaches the workspace offline.
    fn install_cached_session(&self, account_id: &AccountId, profile: AccountProfile) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return;
        }
        state.view = signed_in_view(
            AccountSessionState::Offline,
            account_id,
            profile,
            0,
            "signed in; verifying saved session",
        );
    }

    fn current_restore_refresh_token<V>(&self, vault: &V) -> Result<Option<Zeroizing<String>>, ()>
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let state = self.state.lock().map_err(|_| ())?;
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return Ok(None);
        }
        load_refresh_token(vault)
    }

    fn accept_online_restore_refresh<V>(
        &self,
        vault: &V,
        outcome: Result<oidc::VerifiedTokens, oidc::RefreshGrantError>,
    ) -> Option<oidc::VerifiedTokens>
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let tokens = match outcome {
            Ok(tokens) => tokens,
            Err(oidc::RefreshGrantError::Rejected) => {
                self.retire_rejected_restore(vault);
                return None;
            }
            Err(oidc::RefreshGrantError::Unavailable) => {
                self.complete_restore_without_session(
                    AccountSessionState::TerminalError,
                    "saved sign-in could not be verified; check your connection and sign in again",
                );
                return None;
            }
        };
        // A rotating grant may invalidate the previous token immediately.
        // Persist its verified replacement before anything else, holding the
        // lifecycle fence so sign-out cannot be undone.
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return None;
        }
        if let Some(rotated) = tokens.refresh.as_ref() {
            let secret = Zeroizing::new(rotated.expose().as_bytes().to_vec());
            if vault.store(REFRESH_VAULT_KEY, secret.as_slice()).is_err() {
                let _ = self.retire_unusable_vault_material(vault);
                state.restore_allowed = false;
                state.view = cleared_view(
                    AccountSessionState::TerminalError,
                    0,
                    "credential storage is unavailable; sign in again",
                );
                return None;
            }
        }
        Some(tokens)
    }

    /// Publishes the verified restored session. Returns whether it became
    /// the generation-zero Active session.
    fn apply_online_restore_tokens<V>(&self, vault: &V, tokens: &oidc::VerifiedTokens) -> bool
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok(account_id) = AccountId::try_new(tokens.subject.as_str()) else {
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "saved sign-in could not be verified; sign in again",
            );
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return false;
        }
        if store_profile_material(vault, &account_id, &tokens.profile).is_err() {
            diagnostic!("Aeris account profile cache write degraded");
        }
        state.restore_allowed = false;
        state.view = signed_in_view(
            AccountSessionState::Active,
            &account_id,
            tokens.profile.clone(),
            0,
            "signed in",
        );
        true
    }

    /// Hands a cached offline restore to the session worker after the
    /// identity provider was unreachable. Returns whether a signed-in
    /// generation-zero session remains to be retried.
    fn hand_off_offline_restore(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation != 0
            || state.pending.is_some()
            || state.view.state != AccountSessionState::Offline
        {
            return false;
        }
        state.restore_allowed = false;
        state.view.detail = "signed in offline; reconnecting".to_string();
        true
    }

    /// Retires an authoritatively rejected saved grant and every durable
    /// credential that could otherwise re-authenticate the same restore on a
    /// later process start. The generation-zero lifecycle fence stays held
    /// while deleting so a stale restore cannot erase credentials written by
    /// a newer interactive login.
    fn retire_rejected_restore<V>(&self, vault: &V)
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return;
        }
        let cleanup_complete = self.retire_unusable_vault_material(vault);
        state.restore_allowed = false;
        state.view = if cleanup_complete {
            cleared_view(
                AccountSessionState::ReauthenticationRequired,
                0,
                "saved sign-in expired; sign in again",
            )
        } else {
            cleared_view(
                AccountSessionState::TerminalError,
                0,
                "saved sign-in could not be retired; credential storage is unavailable; sign in again",
            )
        };
    }

    /// Retires an authoritatively rejected active session under the current
    /// generation fence. A partial cleanup keeps the durable rejection marker
    /// and fails the in-memory session closed instead of treating rejection
    /// like an offline network outage.
    fn retire_rejected_session<V>(&self, generation: u64, vault: &V)
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != generation || !is_signed_in(state.view.state) {
            return;
        }
        let cleanup_complete = self.retire_unusable_vault_material(vault);
        state.view = if cleanup_complete {
            cleared_view(
                AccountSessionState::ReauthenticationRequired,
                generation,
                "sign-in expired; sign in again",
            )
        } else {
            cleared_view(
                AccountSessionState::TerminalError,
                generation,
                "sign-in expired but saved credentials could not be retired; sign in again",
            )
        };
    }

    /// Removes every durable credential capable of restoring a session that
    /// must no longer be restorable. Callers hold the account lifecycle state
    /// lock while this runs so a newer interactive session cannot race these
    /// vault mutations. The durable rejection marker doubles as the restart
    /// fence when cleanup cannot complete.
    fn retire_unusable_vault_material<V>(&self, vault: &V) -> bool
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        // Persist the independent rejection fence before mutating vault state
        // so a crash during cleanup cannot make the saved session restorable.
        let marker_persisted = self.persist_rejected_restore_marker().is_ok();
        let refresh_deleted = vault.delete(REFRESH_VAULT_KEY).is_ok();
        let profile_deleted = vault.delete(PROFILE_VAULT_KEY).is_ok();
        let cleaned = refresh_deleted && profile_deleted;
        let marker_settled =
            !marker_persisted || (cleaned && self.clear_rejected_restore_marker().is_ok());
        cleaned && marker_settled
    }

    fn complete_restore_without_session(&self, target: AccountSessionState, detail: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return false;
        }
        // A transient failure never discards a cached verified session; only
        // an authoritative rejection retires it.
        if state.view.state == AccountSessionState::Offline
            && target == AccountSessionState::TerminalError
        {
            state.view.detail = "signed in offline; reconnecting".to_string();
            return true;
        }
        state.restore_allowed = false;
        state.view = cleared_view(target, 0, detail);
        true
    }

    /// Returns whether a verified online or cached-offline session is installed.
    ///
    /// `None` means the account lifecycle state is momentarily busy. Callers
    /// that own an already-authoritative presentation snapshot may retain that
    /// snapshot for the current frame instead of rendering a false sign-out.
    /// Poisoned state remains an authoritative failure and returns `Some(false)`.
    #[must_use]
    pub fn try_is_authenticated(&self) -> Option<bool> {
        match self.state.try_lock() {
            Ok(state) => Some(is_signed_in(state.view.state)),
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(_)) => Some(false),
        }
    }

    /// Returns whether a verified online or cached-offline session is installed.
    ///
    /// This hard authorization probe fails closed when the lifecycle state is
    /// momentarily busy. Presentation code that must avoid a false logout frame
    /// uses [`Self::try_is_authenticated`] and its last authoritative snapshot.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.try_is_authenticated().unwrap_or(false)
    }

    /// Resolves cached OIDC endpoints, refreshing them from discovery once.
    ///
    /// # Errors
    ///
    /// Returns a redacted actionable error when discovery fails.
    fn oidc_endpoints(&self) -> Result<OidcEndpoints, String> {
        if let Ok(cached) = self.endpoints.lock()
            && let Some(endpoints) = cached.clone()
        {
            return Ok(endpoints);
        }
        let discovered = discover(&self.config.issuer, &self.config.hosted_origin)?;
        if let Ok(mut cached) = self.endpoints.lock() {
            *cached = Some(discovered.clone());
        }
        Ok(discovered)
    }

    /// Starts one generation-fenced login transaction and returns the
    /// browser authorization address for the desktop to open.
    ///
    /// Discovery may perform bounded network work, so callers run this off
    /// the UI thread.
    ///
    /// # Errors
    ///
    /// Returns a redacted actionable error when a transaction is already
    /// pending, the generation is retired, discovery fails, or no callback
    /// port can be bound.
    pub fn begin_login(&self, request_generation: u64) -> Result<LoginAuthorization, String> {
        {
            let state = lock_state(&self.state)?;
            if request_generation <= state.last_generation {
                return Err("retired sign-in request; start a new sign-in".to_string());
            }
            if state.pending.is_some() {
                return Err("sign-in is already in progress".to_string());
            }
        }
        let endpoints = self.oidc_endpoints()?;
        let mut state = lock_state(&self.state)?;
        if request_generation <= state.last_generation {
            return Err("retired sign-in request; start a new sign-in".to_string());
        }
        if state.pending.is_some() {
            return Err("sign-in is already in progress".to_string());
        }
        let verifier = PkceVerifier::generate()?;
        let oauth_state = generate_oauth_random()?;
        let nonce = generate_oauth_random()?;
        let listener = LoopbackListener::bind(self.callback_ports)?;
        let redirect_uri = listener.redirect_uri();
        let url = authorization_url(&AuthorizationRequest {
            endpoints: &endpoints,
            client_id: &self.config.client_id,
            redirect_uri: &redirect_uri,
            state: &oauth_state,
            nonce: &nonce,
            code_challenge: &verifier.challenge(),
        })?;
        let expires_at = Instant::now() + LOGIN_TIMEOUT;
        state.last_generation = request_generation;
        state.restore_allowed = false;
        state.pending = Some(PendingLogin {
            generation: request_generation,
            oauth_state,
            nonce,
            verifier,
            redirect_uri,
            expires_at,
            code_received: false,
        });
        state.view = cleared_view(
            AccountSessionState::Authorizing,
            request_generation,
            "waiting for browser authorization",
        );
        let service = self.clone();
        if std::thread::Builder::new()
            .name("aeris-account-login".to_string())
            .spawn(move || service.run_login_transaction(request_generation, &listener))
            .is_err()
        {
            state.pending = None;
            state.view = cleared_view(
                AccountSessionState::SignedOut,
                request_generation,
                "sign-in worker could not start; retry sign-in",
            );
            return Err("sign-in worker could not start; retry sign-in".to_string());
        }
        Ok(LoginAuthorization {
            request_generation,
            authorization_url: url,
            expires_unix_seconds: unix_now() + LOGIN_TIMEOUT.as_secs(),
        })
    }

    /// Cancels one pending login transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching transaction is pending. Retired
    /// generations never mutate current state.
    pub fn cancel_login(&self, request_generation: u64) -> Result<(), String> {
        let mut state = lock_state(&self.state)?;
        match &state.pending {
            Some(pending) if pending.generation == request_generation => {
                let generation = state.last_generation;
                state.pending = None;
                state.view = cleared_view(
                    AccountSessionState::SignedOut,
                    generation,
                    "sign-in cancelled",
                );
                Ok(())
            }
            _ => Err("no matching sign-in transaction is pending".to_string()),
        }
    }

    /// Returns the current sanitized account view, expiring stale logins.
    #[must_use]
    pub fn account_status(&self) -> AccountView {
        let Ok(mut state) = self.state.lock() else {
            return cleared_view(
                AccountSessionState::TerminalError,
                0,
                "account state is unavailable",
            );
        };
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.expires_at)
        {
            let generation = state.last_generation;
            state.pending = None;
            state.view = cleared_view(
                AccountSessionState::SignedOut,
                generation,
                "sign-in timed out; retry sign-in",
            );
        }
        state.view.clone()
    }

    /// Enqueues one best-effort refresh for the currently verified profile
    /// and immediately returns the cached sanitized view. Duplicate requests
    /// collapse while one worker is running.
    #[must_use]
    pub fn request_profile_refresh(&self) -> AccountView {
        let view = self.account_status();
        let Some(generation) = claim_profile_refresh(&view, &self.profile_refresh_in_flight) else {
            return view;
        };

        let service = self.clone();
        if std::thread::Builder::new()
            .name("aeris-account-profile-refresh".to_string())
            .spawn(move || {
                service.run_profile_refresh(generation);
                service
                    .profile_refresh_in_flight
                    .store(false, Ordering::Release);
            })
            .is_err()
        {
            self.profile_refresh_in_flight
                .store(false, Ordering::Release);
        }
        view
    }

    /// Signs out the shared session, deleting vault refresh and profile material.
    ///
    /// Local deletion happens synchronously so a concurrent login cannot
    /// observe stale material; server-side revocation follows on a bounded
    /// worker with the in-memory refresh copy and never blocks the caller.
    ///
    /// # Errors
    ///
    /// Returns an actionable redacted error when saved session credentials
    /// could not be deleted. In-memory session state is still retired before
    /// the error is returned.
    pub fn sign_out(&self) -> Result<AccountView, String> {
        let vault = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE);
        match vault {
            Ok(vault) => self.sign_out_with(&vault),
            Err(_) => self.sign_out_with(&UnavailableVault),
        }
    }

    fn sign_out_with<V>(&self, vault: &V) -> Result<AccountView, String>
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let token = load_refresh_token(vault).unwrap_or_default();
        // Retire every background completion before deleting credentials.
        // Workers take the same state lock while committing vault writes.
        if let Ok(mut state) = self.state.lock() {
            let generation = state.last_generation;
            state.restore_allowed = false;
            state.pending = None;
            state.view = cleared_view(AccountSessionState::SignedOut, generation, "signed out");
        }
        let refresh_deleted = vault.delete(REFRESH_VAULT_KEY).is_ok();
        let profile_deleted = vault.delete(PROFILE_VAULT_KEY).is_ok();
        // Revocation reuses already-cached endpoints only: sign-out never
        // performs discovery on the caller's path.
        let cached = self
            .endpoints
            .lock()
            .map(|cached| cached.clone())
            .unwrap_or_default();
        let revocation = cached.zip(token);
        let deleted = refresh_deleted && profile_deleted;
        let view = {
            let Ok(mut state) = self.state.lock() else {
                return Err("account state is unavailable".to_string());
            };
            // Profile leaves with the session: a later sign-in as another
            // user never inherits these fields, even briefly.
            state.view = cleared_view(
                AccountSessionState::SignedOut,
                state.last_generation,
                if deleted {
                    "signed out"
                } else {
                    "signed out; credential cleanup needs attention"
                },
            );
            state.view.clone()
        };
        if let Some((endpoints, token)) = revocation {
            let client_id = self.config.client_id.clone();
            std::thread::Builder::new()
                .name("aeris-account-revoke".to_string())
                .spawn(move || {
                    if oidc::revoke_refresh(&endpoints, &client_id, &token).is_err() {
                        diagnostic!("Aeris sign-out revocation degraded");
                    }
                })
                .ok();
        }
        if deleted {
            Ok(view)
        } else {
            Err("sign-out could not remove saved credentials; retry sign-out".to_string())
        }
    }

    /// Runs one login transaction to a truthful browser page. The loopback
    /// connection stays open while the runtime exchanges, verifies, and
    /// stores; the browser then reads the completed outcome, never mere
    /// receipt. Processing is bounded by the endpoint timeouts plus local
    /// vault work, and every path answers the waiting browser exactly once.
    fn run_login_transaction(&self, generation: u64, listener: &LoopbackListener) {
        let remaining = self.pending_remaining(generation);
        if remaining.is_zero() {
            self.fail_generation(generation, "sign-in timed out; retry sign-in");
            return;
        }
        let callback = match listener
            .accept_one_while(remaining, || self.login_generation_is_pending(generation))
        {
            Ok(callback) => callback,
            Err(detail) => {
                self.fail_generation(generation, &detail);
                return;
            }
        };
        let code = match self.apply_callback_query(callback.query(), generation) {
            Ok(code) => code,
            Err(detail) => {
                callback.respond_invalid(&detail);
                self.fail_generation(generation, &detail);
                return;
            }
        };
        match self.exchange_and_store(generation, &code) {
            Ok(true) => callback.respond_outcome(Ok(())),
            Ok(false) => {
                // Vault failure or a retired generation: the view already
                // carries the truth, so the page repeats it verbatim.
                let detail = self.account_status().detail;
                callback.respond_outcome(Err(detail.as_str()));
            }
            Err(detail) => {
                callback.respond_outcome(Err(detail.as_str()));
                self.fail_generation(generation, &detail);
            }
        }
    }

    fn login_generation_is_pending(&self, generation: u64) -> bool {
        self.state.lock().is_ok_and(|state| {
            state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.generation == generation)
        })
    }

    fn pending_remaining(&self, generation: u64) -> Duration {
        let Ok(state) = self.state.lock() else {
            return Duration::ZERO;
        };
        state
            .pending
            .as_ref()
            .filter(|pending| pending.generation == generation)
            .map(|pending| pending.expires_at.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    }

    fn apply_callback_query(&self, query: &str, generation: u64) -> Result<String, String> {
        let mut state = lock_state(&self.state)?;
        let pending = state
            .pending
            .as_mut()
            .filter(|pending| pending.generation == generation);
        let Some(pending) = pending else {
            return Err("no matching sign-in transaction is pending".to_string());
        };
        if pending.code_received {
            return Err("duplicate authorization callback".to_string());
        }
        let validated = validate_callback_query(query, &pending.oauth_state)?;
        pending.code_received = true;
        state.view.detail = "completing sign-in".to_string();
        Ok(validated.code)
    }

    /// Exchanges, verifies, and stores one transaction. Returns whether the
    /// Active session published: `Ok(true)` completes the sign-in,
    /// `Ok(false)` leaves an already-recorded terminal failure (vault
    /// failure) or a retired generation untouched, and `Err` carries the
    /// redacted detail for the browser page and the desktop account view.
    fn exchange_and_store(&self, generation: u64, code: &str) -> Result<bool, String> {
        let (redirect_uri, verifier, nonce) = {
            let Ok(state) = self.state.lock() else {
                return Ok(false);
            };
            let Some(pending) = state
                .pending
                .as_ref()
                .filter(|pending| pending.generation == generation)
            else {
                return Ok(false);
            };
            (
                pending.redirect_uri.clone(),
                Zeroizing::new(pending.verifier.encoded().to_string()),
                pending.nonce.clone(),
            )
        };
        let endpoints = self.oidc_endpoints()?;
        // One connection pool for the whole transaction: the exchange and JWKS
        // verification reuse it instead of paying a fresh TLS handshake.
        let agent = oidc_agent();
        let tokens = exchange_code(
            &endpoints,
            &agent,
            &self.config.client_id,
            &redirect_uri,
            code,
            &verifier,
            &nonce,
        )?;
        let account_id = AccountId::try_new(tokens.subject.as_str())
            .map_err(|_| "sign-in verification failed; retry sign-in".to_string())?;
        let Some(refresh) = tokens.refresh.as_ref() else {
            return Err("sign-in did not return a saved session; retry sign-in".to_string());
        };
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            self.fail_generation(
                generation,
                "credential storage is unavailable; retry sign-in",
            );
            return Ok(false);
        };
        Ok(self.complete_with_tokens(
            generation,
            &account_id,
            &tokens.profile,
            refresh.expose(),
            &vault,
        ))
    }

    /// Publishes the Active session only after the refresh and profile
    /// material are stored: exchange, verification, and vault all precede any
    /// success the desktop can render. A retired generation assigns nothing,
    /// so a late callback can never restore another user's profile. Returns
    /// whether the Active session published.
    fn complete_with_tokens<V>(
        &self,
        generation: u64,
        account_id: &AccountId,
        profile: &AccountProfile,
        refresh_token: &str,
        vault: &V,
    ) -> bool
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.generation != generation)
        {
            return false;
        }
        let secret = Zeroizing::new(refresh_token.as_bytes().to_vec());
        let stored = !secret.is_empty()
            && vault.store(REFRESH_VAULT_KEY, secret.as_slice()).is_ok()
            && store_profile_material(vault, account_id, profile).is_ok();
        if !stored {
            let _ = self.retire_unusable_vault_material(vault);
        }
        let marker_cleared = stored && self.clear_rejected_restore_marker().is_ok();
        state.pending = None;
        if !marker_cleared {
            state.view = cleared_view(
                AccountSessionState::TerminalError,
                generation,
                if stored {
                    "saved sign-in cleanup is unavailable; retry sign-in"
                } else {
                    "credential storage is unavailable; retry sign-in"
                },
            );
            return false;
        }
        state.view = signed_in_view(
            AccountSessionState::Active,
            account_id,
            profile.clone(),
            generation,
            "signed in",
        );
        drop(state);
        self.spawn_session_worker(generation, SESSION_REFRESH_INTERVAL);
        true
    }

    /// Returns whether one generation still owns a signed-in shared session.
    fn is_current(&self, generation: u64) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.last_generation == generation && is_signed_in(state.view.state)
        })
    }

    fn spawn_session_worker(&self, generation: u64, first_delay: Duration) {
        let service = self.clone();
        if std::thread::Builder::new()
            .name("aeris-account-session".to_string())
            .spawn(move || service.run_session_worker(generation, first_delay))
            .is_err()
        {
            // The session stays signed in from verified state; only periodic
            // reverification is lost until the next start.
            diagnostic!("Aeris account session worker could not start");
        }
    }

    /// Reverifies the session on a jittered interval and retries with bounded
    /// backoff while the identity provider is unreachable. Exits when its
    /// generation retires.
    fn run_session_worker(&self, generation: u64, first_delay: Duration) {
        let mut delay = first_delay;
        let mut backoff = OfflineBackoff::new();
        loop {
            if !self.sleep_while_current(generation, delay) {
                return;
            }
            delay = match self.session_round(generation) {
                SessionRefresh::Verified => {
                    backoff = OfflineBackoff::new();
                    session_refresh_wait(random_jitter())
                }
                SessionRefresh::Deferred => QUIESCED_RETRY,
                SessionRefresh::Unavailable => backoff.next_delay(),
                SessionRefresh::Retired => return,
            };
        }
    }

    /// Sleeps in short slices so a retired generation stops promptly.
    fn sleep_while_current(&self, generation: u64, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        loop {
            if !self.is_current(generation) {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            std::thread::sleep(Duration::from_secs(1).min(deadline - now));
        }
    }

    fn session_round(&self, generation: u64) -> SessionRefresh {
        let Ok(_refresh_guard) = self.refresh_gate.lock() else {
            return SessionRefresh::Retired;
        };
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            return SessionRefresh::Unavailable;
        };
        let outcome = refresh_session_round(self, generation, &vault);
        apply_session_outcome(self, generation, outcome);
        outcome
    }

    fn run_profile_refresh(&self, generation: u64) {
        let Ok(_refresh_guard) = self.refresh_gate.lock() else {
            return;
        };
        if !self.is_current(generation) {
            return;
        }
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            return;
        };
        // A cosmetic refresh applies only verified outcomes: transient
        // unavailability belongs to the session worker's retry cadence, while
        // rejection and storage failures retire state inside the shared round.
        let outcome = refresh_session_round(self, generation, &vault);
        if outcome == SessionRefresh::Verified {
            apply_session_outcome(self, generation, outcome);
        }
    }

    /// Replaces the session profile with one from a freshly verified ID token
    /// for the same account and generation.
    fn apply_verified_profile<V>(
        &self,
        vault: &V,
        generation: u64,
        expected_account: &AccountId,
        tokens: &oidc::VerifiedTokens,
    ) -> bool
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        if tokens.subject != expected_account.as_str() {
            diagnostic!("Aeris account refresh returned another subject");
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation != generation
            || state.view.account_id != expected_account.as_str()
            || !is_signed_in(state.view.state)
        {
            return false;
        }
        if store_profile_material(vault, expected_account, &tokens.profile).is_err() {
            diagnostic!("Aeris account profile cache write degraded");
        }
        state
            .view
            .display_name
            .clone_from(&tokens.profile.display_name);
        state.view.email.clone_from(&tokens.profile.email);
        state.view.photo_url.clone_from(&tokens.profile.photo_url);
        true
    }

    fn fail_generation(&self, generation: u64, detail: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.generation != generation)
        {
            return;
        }
        state.pending = None;
        let generation = state.last_generation;
        state.view = if detail.contains("timed out") || detail.contains("cancelled") {
            cleared_view(AccountSessionState::SignedOut, generation, detail)
        } else {
            cleared_view(AccountSessionState::TerminalError, generation, detail)
        };
    }
}

const fn is_signed_in(state: AccountSessionState) -> bool {
    matches!(
        state,
        AccountSessionState::Active | AccountSessionState::Offline
    )
}

fn claim_profile_refresh(view: &AccountView, in_flight: &AtomicBool) -> Option<u64> {
    if !is_signed_in(view.state) {
        return None;
    }
    in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| view.request_generation)
}

/// One sanitized view with identity, plan, and profile cleared. Every
/// signed-out assignment flows through here so sign-out, cancel, expiry,
/// and failure can never leak a previous user's profile.
fn cleared_view(state: AccountSessionState, generation: u64, detail: &str) -> AccountView {
    AccountView {
        state,
        account_id: String::new(),
        plan_id: String::new(),
        detail: detail.to_string(),
        request_generation: generation,
        display_name: String::new(),
        email: String::new(),
        photo_url: String::new(),
    }
}

fn signed_in_view(
    state: AccountSessionState,
    account_id: &AccountId,
    profile: AccountProfile,
    generation: u64,
    detail: &str,
) -> AccountView {
    AccountView {
        state,
        account_id: account_id.as_str().to_string(),
        plan_id: PlanId::EarlyAccess.as_str().to_string(),
        detail: detail.to_string(),
        request_generation: generation,
        display_name: profile.display_name,
        email: profile.email,
        photo_url: profile.photo_url,
    }
}

fn lock_state(
    state: &Arc<Mutex<ServiceState>>,
) -> Result<std::sync::MutexGuard<'_, ServiceState>, String> {
    state
        .lock()
        .map_err(|_| "account state is unavailable".to_string())
}

fn random_jitter() -> Option<[u8; 8]> {
    let mut jitter = [0_u8; 8];
    getrandom::fill(&mut jitter).is_ok().then_some(jitter)
}

fn session_refresh_wait(jitter: Option<[u8; 8]>) -> Duration {
    SESSION_REFRESH_INTERVAL
        + Duration::from_secs(jitter.map_or(0, |bytes| {
            u64::from_le_bytes(bytes) % SESSION_REFRESH_JITTER_SECONDS
        }))
}

struct SessionCredentials {
    account_id: AccountId,
    refresh_token: Zeroizing<String>,
    endpoints: OidcEndpoints,
}

fn session_credentials<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
) -> Result<SessionCredentials, SessionRefresh>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    // Resolve network-owned endpoint metadata before taking the lifecycle lock.
    // Credential selection below must then remain under one generation fence:
    // otherwise a stale worker could consume a newer login's rotating token.
    let endpoints = service
        .oidc_endpoints()
        .map_err(|_| SessionRefresh::Unavailable)?;
    let state = service.state.lock().map_err(|_| SessionRefresh::Retired)?;
    if state.last_generation != generation || !is_signed_in(state.view.state) {
        return Err(SessionRefresh::Retired);
    }
    let account_id =
        AccountId::try_new(&state.view.account_id).map_err(|_| SessionRefresh::Retired)?;
    let Ok(Some(refresh_token)) = load_refresh_token(vault) else {
        drop(state);
        fail_current_storage(service, generation);
        return Err(SessionRefresh::Retired);
    };
    Ok(SessionCredentials {
        account_id,
        refresh_token,
        endpoints,
    })
}

fn persist_rotated_refresh<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
    refresh_token: Option<&oidc::SecretToken>,
) -> bool
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Some(refresh_token) = refresh_token else {
        return service.is_current(generation);
    };
    let secret = Zeroizing::new(refresh_token.expose().as_bytes().to_vec());
    let Ok(mut state) = service.state.lock() else {
        return false;
    };
    if state.last_generation != generation || !is_signed_in(state.view.state) {
        return false;
    }
    if vault.store(REFRESH_VAULT_KEY, secret.as_slice()).is_err() {
        // A successful rotating grant may have invalidated the durable token
        // still stored. Retire every restorable artifact under this same
        // generation fence so a restart cannot resurrect the unusable session.
        let _ = service.retire_unusable_vault_material(vault);
        state.restore_allowed = false;
        state.view = cleared_view(
            AccountSessionState::TerminalError,
            generation,
            "credential storage is unavailable; sign in again",
        );
        return false;
    }
    true
}

fn accept_refresh_grant<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
    outcome: Result<oidc::VerifiedTokens, oidc::RefreshGrantError>,
) -> Result<oidc::VerifiedTokens, SessionRefresh>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    match outcome {
        Ok(tokens) => {
            if persist_rotated_refresh(service, generation, vault, tokens.refresh.as_ref()) {
                Ok(tokens)
            } else {
                Err(SessionRefresh::Retired)
            }
        }
        Err(oidc::RefreshGrantError::Rejected) => {
            note_session("refresh-rejected");
            service.retire_rejected_session(generation, vault);
            Err(SessionRefresh::Retired)
        }
        Err(oidc::RefreshGrantError::Unavailable) => Err(SessionRefresh::Unavailable),
    }
}

fn fail_current_storage(service: &AccountService, generation: u64) {
    let Ok(mut state) = service.state.lock() else {
        return;
    };
    if state.last_generation != generation || !is_signed_in(state.view.state) {
        return;
    }
    state.restore_allowed = false;
    state.view = cleared_view(
        AccountSessionState::TerminalError,
        generation,
        "credential storage is unavailable; sign in again",
    );
}

/// One background refresh round: refresh grant, durable rotation, verified
/// profile replacement.
fn refresh_session_round<V>(service: &AccountService, generation: u64, vault: &V) -> SessionRefresh
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let session = match session_credentials(service, generation, vault) {
        Ok(session) => session,
        Err(outcome) => return outcome,
    };
    let agent = oidc_agent();
    let grant_permit = match service.claim_refresh_grant() {
        Ok(Some(permit)) => permit,
        Ok(None) => return SessionRefresh::Deferred,
        Err(_) => return SessionRefresh::Unavailable,
    };
    let refresh = refresh_grant(
        &session.endpoints,
        &agent,
        &service.config.client_id,
        &session.refresh_token,
    );
    let tokens = match accept_refresh_grant(service, generation, vault, refresh) {
        Ok(tokens) => tokens,
        Err(outcome) => return outcome,
    };
    drop(grant_permit);
    if service.apply_verified_profile(vault, generation, &session.account_id, &tokens) {
        SessionRefresh::Verified
    } else {
        SessionRefresh::Unavailable
    }
}

/// Moves a current session between verified-online and offline presentation.
fn apply_session_outcome(service: &AccountService, generation: u64, outcome: SessionRefresh) {
    let Ok(mut state) = service.state.lock() else {
        return;
    };
    if state.last_generation != generation || !is_signed_in(state.view.state) {
        return;
    }
    match outcome {
        SessionRefresh::Verified => {
            state.view.state = AccountSessionState::Active;
            state.view.detail = "signed in".to_string();
            note_session("verified");
        }
        SessionRefresh::Unavailable => {
            state.view.state = AccountSessionState::Offline;
            state.view.detail = "signed in offline; reconnecting".to_string();
            note_session("offline");
        }
        SessionRefresh::Deferred => note_session("deferred"),
        SessionRefresh::Retired => {}
    }
}

/// Redacted session observation: outcome class only, never identities.
fn note_session(outcome: &str) {
    diagnostic!("Aeris account session: {outcome}");
}

/// Credential vault that is always unavailable, used when native storage
/// cannot be constructed so sign-out still clears runtime state.
struct UnavailableVault;

impl CredentialVault for UnavailableVault {
    type Error = String;

    fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
        Err("credential storage is unavailable".to_string())
    }

    fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        Err("credential storage is unavailable".to_string())
    }

    fn delete(&self, _key: &str) -> Result<(), Self::Error> {
        Err("credential storage is unavailable".to_string())
    }
}

fn load_refresh_token<V>(vault: &V) -> Result<Option<Zeroizing<String>>, ()>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Some(bytes) = vault.load(REFRESH_VAULT_KEY).map_err(|_| ())? else {
        return Ok(None);
    };
    Ok(String::from_utf8(bytes)
        .ok()
        .filter(|token| !token.is_empty())
        .map(Zeroizing::new))
}

fn store_profile_material<V>(
    vault: &V,
    account_id: &AccountId,
    profile: &AccountProfile,
) -> Result<(), String>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let encoded = serde_json::to_vec(&CachedAccountProfile {
        schema_version: PROFILE_VAULT_SCHEMA_VERSION,
        account_id: account_id.as_str().to_string(),
        display_name: profile.display_name.clone(),
        email: profile.email.clone(),
        photo_url: profile.photo_url.clone(),
    })
    .map_err(|_| "credential storage is unavailable".to_string())?;
    if encoded.len() > MAXIMUM_PROFILE_VAULT_BYTES {
        return Err("credential storage is unavailable".to_string());
    }
    let encoded = Zeroizing::new(encoded);
    vault
        .store(PROFILE_VAULT_KEY, encoded.as_slice())
        .map_err(|_| "credential storage is unavailable".to_string())
}

/// Loads the cached verified profile. Malformed or out-of-bound entries read
/// as absent; only a vault read failure is an error.
fn load_profile_material<V>(vault: &V) -> Result<Option<(AccountId, AccountProfile)>, ()>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Some(encoded) = vault.load(PROFILE_VAULT_KEY).map_err(|_| ())? else {
        return Ok(None);
    };
    let encoded = Zeroizing::new(encoded);
    if encoded.is_empty() || encoded.len() > MAXIMUM_PROFILE_VAULT_BYTES {
        return Ok(None);
    }
    let Ok(cached) = serde_json::from_slice::<CachedAccountProfile>(&encoded) else {
        return Ok(None);
    };
    if cached.schema_version != PROFILE_VAULT_SCHEMA_VERSION
        || cached.display_name.len() > MAXIMUM_PROFILE_NAME_BYTES
        || cached.email.len() > MAXIMUM_PROFILE_EMAIL_BYTES
        || cached.photo_url.len() > MAXIMUM_PROFILE_PHOTO_URL_BYTES
    {
        return Ok(None);
    }
    let Ok(account_id) = AccountId::try_new(cached.account_id) else {
        return Ok(None);
    };
    Ok(Some((
        account_id,
        AccountProfile {
            display_name: cached.display_name,
            email: cached.email,
            photo_url: cached.photo_url,
        },
    )))
}

#[cfg(unix)]
fn sync_account_marker_directory(path: &std::path::Path) -> Result<(), ()> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests;
