//! Engine-owned native account session for phase 5 authentication.
//!
//! One [`AccountService`] is shared by all desktop windows. It owns the
//! PKCE transaction, the ephemeral loopback listener, the OIDC exchange,
//! vault material, and the generation-fenced session state. It owns no
//! market-coordinator, provider, or chart state, and account refresh never
//! recreates a provider session.

pub mod lease;
pub mod loopback;
pub mod oidc;
pub mod pkce;

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

use axiusflow_account::{
    AccountId, MAXIMUM_PROFILE_EMAIL_BYTES, MAXIMUM_PROFILE_NAME_BYTES,
    MAXIMUM_PROFILE_PHOTO_URL_BYTES, PlanId,
};
use axiusflow_contracts::{AccountSessionState, AccountView, LoginAuthorization};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use zeroize::Zeroizing;

use lease::{LEASE_OFFLINE_VALIDITY_SECONDS, LEASE_REFRESH_INTERVAL_SECONDS, device_id_for_key};
use loopback::{LoopbackListener, validate_callback_query};
use oidc::{
    AccountProfile, AuthorizationRequest, OidcEndpoints, authorization_url, control_plane_origin,
    discover, exchange_code, link_subject, oidc_agent, refresh_grant,
};
use pkce::{PkceVerifier, generate_oauth_random};

/// How long one login transaction waits for the browser callback.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// Native public client identifier registered with the control plane.
pub const NATIVE_CLIENT_ID: &str = "axiusflow-desktop";
/// Default control-plane OIDC issuer (the Better Auth mount).
pub const DEFAULT_AUTH_ISSUER: &str = "https://auth.axiusflow.com/api/auth";

const ACCOUNT_VAULT_SERVICE: &str = "com.axiusflow.account";
const REFRESH_VAULT_KEY: &str = "account-refresh-default-v1";
const LEASE_VAULT_KEY: &str = "account-entitlement-lease-v1";
const LEASE_DIRECTORY_VAULT_KEY: &str = "account-entitlement-directory-v1";
const DEVICE_VAULT_KEY: &str = "account-device-key-v1";
const PROFILE_VAULT_KEY: &str = "account-profile-v1";
const REJECTED_RESTORE_MARKER_FILE: &str = "account-restore-rejected-v1";
const PROFILE_VAULT_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_PROFILE_VAULT_BYTES: usize = 4096;

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
/// Remote OIDC refresh/link work remains asynchronous and is intentionally not
/// part of candidate activation health.
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

/// Control-plane configuration for one account service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountServiceConfig {
    issuer: String,
    client_id: String,
}

impl AccountServiceConfig {
    /// Creates a validated control-plane configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the issuer is not an `https` origin or the
    /// client identifier is empty.
    pub fn try_new(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Result<Self, String> {
        let issuer = issuer.into();
        let client_id = client_id.into();
        if !issuer.starts_with("https://") || client_id.trim().is_empty() {
            return Err("account control plane configuration is invalid".to_string());
        }
        Ok(Self { issuer, client_id })
    }

    /// Reads deployment configuration from the environment.
    #[must_use]
    pub fn from_environment() -> Self {
        let issuer = std::env::var("AXIUSFLOW_AUTH_ISSUER")
            .unwrap_or_else(|_| DEFAULT_AUTH_ISSUER.to_string());
        Self {
            issuer,
            client_id: NATIVE_CLIENT_ID.to_string(),
        }
    }

    /// Returns whether this configuration can start a login transaction.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.issuer.starts_with("https://") && !self.client_id.trim().is_empty()
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
    lease_expires_at: Option<u64>,
}

type LeaseKey = (String, [u8; 32]);

#[derive(Clone)]
enum RejectedRestoreMarker {
    #[cfg(test)]
    Disabled,
    Unavailable,
    Path(PathBuf),
}

/// Engine-owned account session shared by all desktop windows.
#[derive(Clone)]
pub struct AccountService {
    config: AccountServiceConfig,
    rejected_restore_marker: RejectedRestoreMarker,
    state: Arc<Mutex<ServiceState>>,
    endpoints: Arc<Mutex<Option<OidcEndpoints>>>,
    lease_keys: Arc<Mutex<Vec<LeaseKey>>>,
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
    ContinueOnline { cached_expiry: Option<u64> },
}

impl AccountService {
    /// Creates an account session starting signed out.
    #[must_use]
    pub fn new(config: AccountServiceConfig) -> Self {
        let rejected_restore_marker = axiusflow_platform_runtime::native_data_root()
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
            rejected_restore_marker,
            state: Arc::new(Mutex::new(ServiceState {
                view: cleared_view(AccountSessionState::SignedOut, 0, "signed out"),
                pending: None,
                last_generation: 0,
                restore_allowed: true,
                lease_expires_at: None,
            })),
            endpoints: Arc::new(Mutex::new(None)),
            lease_keys: Arc::new(Mutex::new(Vec::new())),
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

    /// Creates the production account service and attempts a server-verified
    /// restore from native-vault refresh material on a bounded worker.
    ///
    /// A previously verified, still-valid offline lease may cover startup while
    /// the online refresh/link path runs. Cached access is revalidated against
    /// the last verified signing directory and native device binding first.
    #[must_use]
    pub fn new_restoring(config: AccountServiceConfig) -> Self {
        let service = Self::new(config);
        service.start_restore();
        service
    }

    /// Starts production saved-session restoration once for this shared
    /// service. This can be called after process-lifecycle guards are installed
    /// so no refresh grant exists before native shutdown fencing is ready.
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
        if !self.config.is_configured() {
            self.set_restore_readiness(AccountRestoreReadiness::Failed);
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "sign-in is unavailable; the account service is not configured",
            );
            return;
        }
        let restoring = self.clone();
        if std::thread::Builder::new()
            .name("axiusflow-account-restore".to_string())
            .spawn(move || restoring.restore_online_session())
            .is_err()
        {
            self.set_restore_readiness(AccountRestoreReadiness::Failed);
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "saved sign-in restore could not start; retry sign-in",
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
    /// for any grant that already reached the control plane. The waiter covers
    /// the grant through its mandatory rotated-refresh-token vault commit.
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
                "credential storage is unavailable; retry sign-in",
            );
            return;
        };
        let LocalRestore::ContinueOnline { cached_expiry, .. } = self.restore_local_session(&vault)
        else {
            return;
        };
        // Candidate readiness ends at the local-vault boundary above. Everything
        // below may perform bounded remote work and must not make activation
        // depend on current control-plane reachability.
        let agent = oidc_agent();
        let endpoints = self.retry_restore_lookup(|| self.oidc_endpoints());
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
                    break None;
                }
                Err(_) => break None,
            };
            let refresh_token = match self.current_restore_refresh_token(&vault) {
                Ok(Some(refresh_token)) => refresh_token,
                Ok(None) => break None,
                Err(()) => {
                    self.set_restore_readiness(AccountRestoreReadiness::Failed);
                    self.complete_restore_without_session(
                        AccountSessionState::TerminalError,
                        "credential storage is unavailable; retry sign-in",
                    );
                    if let Some(expires_at) = cached_expiry {
                        self.spawn_cached_lease_expiry_worker(expires_at);
                    }
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
            // vault commit. Both guards release before unrelated profile/lease
            // network work continues. A cancelled update quiesce lets this
            // generation-zero restore resume instead of abandoning its cadence.
            let tokens = self.accept_online_restore_refresh(&vault, outcome);
            drop(permit);
            break tokens;
        };
        let Some(tokens) = tokens else {
            if self
                .state
                .lock()
                .is_ok_and(|state| state.view.state == AccountSessionState::OfflineLease)
                && let Some(expires_at) = cached_expiry
            {
                self.spawn_cached_lease_expiry_worker(expires_at);
            }
            return;
        };
        let Some((account_id, tokens)) =
            self.apply_online_restore_tokens(&vault, tokens, |tokens| {
                let endpoints = endpoints.as_ref().map_err(Clone::clone)?;
                self.retry_restore_lookup(|| {
                    link_subject(endpoints, &agent, &tokens.id_token, &tokens.subject)
                })
            })
        else {
            if self
                .state
                .lock()
                .is_ok_and(|state| state.view.state == AccountSessionState::OfflineLease)
                && let Some(expires_at) = cached_expiry
            {
                self.spawn_cached_lease_expiry_worker(expires_at);
            }
            return;
        };
        self.refresh_lease_once(0, &tokens, &account_id, &agent, &vault);
        if !self.spawn_lease_worker(0) {
            self.fail_lease_worker_start(0);
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
                "saved sign-in could not be retired; credential storage is unavailable; retry sign-in",
            );
            return LocalRestore::Settled;
        }
        let refresh_material = vault.load(REFRESH_VAULT_KEY);
        let (cached_expiry, cached_lease_read_failed) =
            if let Ok(expiry) = self.restore_cached_lease(vault) {
                (expiry, false)
            } else {
                // Candidate activation must fail closed when native cached-session
                // storage is not fully readable. Normal production restore keeps
                // the pre-existing behavior: a separately readable refresh token
                // may still recover the session online.
                self.set_restore_readiness(AccountRestoreReadiness::Failed);
                (None, true)
            };
        let cached_session = cached_expiry.is_some();
        let refresh_token = match refresh_material {
            Ok(Some(bytes)) => String::from_utf8(bytes)
                .ok()
                .filter(|token| !token.is_empty())
                .map(Zeroizing::new),
            Ok(None) => {
                if !cached_session {
                    if !self.complete_restore_without_session(
                        AccountSessionState::SignedOut,
                        "signed out",
                    ) {
                        self.set_restore_readiness(AccountRestoreReadiness::Failed);
                        return LocalRestore::Settled;
                    }
                } else if let Some(expires_at) = cached_expiry {
                    self.spawn_cached_lease_expiry_worker(expires_at);
                }
                if !cached_lease_read_failed {
                    self.set_restore_readiness(AccountRestoreReadiness::Ready);
                }
                return LocalRestore::Settled;
            }
            Err(_) => {
                self.set_restore_readiness(AccountRestoreReadiness::Failed);
                self.complete_restore_without_session(
                    AccountSessionState::TerminalError,
                    "credential storage is unavailable; retry sign-in",
                );
                if let Some(expires_at) = cached_expiry {
                    self.spawn_cached_lease_expiry_worker(expires_at);
                }
                return LocalRestore::Settled;
            }
        };
        let Some(_refresh_token) = refresh_token else {
            if !self.complete_restore_without_session(
                AccountSessionState::ReauthenticationRequired,
                "saved sign-in expired; sign in again",
            ) {
                self.set_restore_readiness(AccountRestoreReadiness::Failed);
                return LocalRestore::Settled;
            }
            if !cached_lease_read_failed {
                self.set_restore_readiness(AccountRestoreReadiness::Ready);
            }
            return LocalRestore::Settled;
        };
        if !cached_lease_read_failed {
            self.set_restore_readiness(AccountRestoreReadiness::Ready);
        }
        LocalRestore::ContinueOnline { cached_expiry }
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
        vault
            .load(REFRESH_VAULT_KEY)
            .map_err(|_| ())
            .map(|material| {
                material
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .filter(|token| !token.is_empty())
                    .map(Zeroizing::new)
            })
    }

    fn restore_cached_lease<V>(&self, vault: &V) -> Result<Option<u64>, ()>
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let compact = match vault.load(LEASE_VAULT_KEY).map_err(|_| ())? {
            Some(bytes) => String::from_utf8(bytes)
                .ok()
                .filter(|lease| !lease.is_empty() && lease.len() <= lease::MAXIMUM_LEASE_BYTES),
            None => None,
        };
        let Some(compact) = compact else {
            return Ok(None);
        };
        let directory = vault
            .load(LEASE_DIRECTORY_VAULT_KEY)
            .map_err(|_| ())?
            .and_then(|encoded| serde_json::from_slice::<Vec<LeaseKey>>(&encoded).ok())
            .filter(|keys| !keys.is_empty());
        let Some(directory) = directory else {
            return Ok(None);
        };
        let device_id = vault
            .load(DEVICE_VAULT_KEY)
            .map_err(|_| ())?
            .filter(|key| !key.is_empty())
            .map(|key| device_id_for_key(&key));
        let Some(device_id) = device_id else {
            return Ok(None);
        };
        let Ok(account_id) = lease::untrusted_account_id(&compact) else {
            return Ok(None);
        };
        let now = unix_now();
        let Ok(claims) =
            lease::validate_compact(&compact, &directory, &account_id, &device_id, now)
        else {
            return Ok(None);
        };
        if claims.expires_at().saturating_sub(now) > LEASE_OFFLINE_VALIDITY_SECONDS {
            return Ok(None);
        }
        let Some(profile) = load_profile_material(vault, &account_id)? else {
            // A cached lease is entitlement authority, not a complete desktop
            // identity. Older/incomplete saved sessions must finish verified
            // online restore before they can skip onboarding.
            return Ok(None);
        };
        let Ok(mut state) = self.state.lock() else {
            return Err(());
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return Ok(None);
        }
        let Ok(mut cached_keys) = self.lease_keys.lock() else {
            return Err(());
        };
        cached_keys.clone_from(&directory);
        drop(cached_keys);
        state.view = AccountView {
            state: AccountSessionState::OfflineLease,
            account_id: account_id.as_str().to_string(),
            plan_id: claims.plan().as_str().to_string(),
            detail: "signed in with cached access; reconnecting to refresh".to_string(),
            request_generation: 0,
            display_name: profile.display_name,
            email: profile.email,
            photo_url: profile.photo_url,
        };
        state.lease_expires_at = Some(claims.expires_at());
        Ok(Some(claims.expires_at()))
    }

    fn spawn_cached_lease_expiry_worker(&self, expires_at_unix_seconds: u64) {
        let service = self.clone();
        std::thread::Builder::new()
            .name("axiusflow-account-cached-expiry".to_string())
            .spawn(move || service.run_cached_lease_expiry_worker(expires_at_unix_seconds))
            .ok();
    }

    fn run_cached_lease_expiry_worker(&self, expires_at_unix_seconds: u64) {
        while unix_now() < expires_at_unix_seconds {
            if !self.state.lock().is_ok_and(|state| {
                state.last_generation == 0
                    && state.pending.is_none()
                    && state.view.state == AccountSessionState::OfflineLease
            }) {
                return;
            }
            let remaining = expires_at_unix_seconds.saturating_sub(unix_now());
            std::thread::sleep(Duration::from_secs(remaining.clamp(1, 60)));
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation == 0
            && state.pending.is_none()
            && state.view.state == AccountSessionState::OfflineLease
        {
            expire_lease_if_needed(&mut state, unix_now());
        }
    }

    #[cfg(test)]
    fn apply_online_restore<V>(
        &self,
        vault: &V,
        outcome: Result<oidc::VerifiedTokens, oidc::RefreshGrantError>,
        link: impl FnOnce(&oidc::VerifiedTokens) -> Result<(AccountId, PlanId, AccountProfile), String>,
    ) -> Option<(AccountId, oidc::VerifiedTokens)>
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let tokens = self.accept_online_restore_refresh(vault, outcome)?;
        self.apply_online_restore_tokens(vault, tokens, link)
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
            Err(error) => {
                match error {
                    oidc::RefreshGrantError::Rejected => {
                        self.retire_rejected_restore(vault);
                    }
                    oidc::RefreshGrantError::Unavailable => {
                        self.complete_restore_without_session(
                            AccountSessionState::TerminalError,
                            "saved sign-in could not be verified; check your connection and retry sign-in",
                        );
                    }
                }
                return None;
            }
        };
        // A refresh grant may invalidate the previous token immediately. Persist
        // its verified replacement before another fallible network operation.
        // Hold the lifecycle fence while writing so sign-out cannot be undone.
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return None;
        }
        if let Some(rotated) = tokens.refresh.as_deref().filter(|token| !token.is_empty()) {
            let secret = Zeroizing::new(rotated.as_bytes().to_vec());
            if vault.store(REFRESH_VAULT_KEY, secret.as_slice()).is_err() {
                // The successful grant may already have invalidated the old
                // durable refresh token. Retire every artifact that could
                // restore that stale session, using the same durable marker
                // fallback as active-session rotation failures.
                let _ = self.retire_unusable_vault_material(vault);
                state.restore_allowed = false;
                state.lease_expires_at = None;
                state.view = cleared_view(
                    AccountSessionState::TerminalError,
                    0,
                    "credential storage is unavailable; retry sign-in",
                );
                return None;
            }
        }
        Some(tokens)
    }

    fn apply_online_restore_tokens<V>(
        &self,
        vault: &V,
        tokens: oidc::VerifiedTokens,
        link: impl FnOnce(&oidc::VerifiedTokens) -> Result<(AccountId, PlanId, AccountProfile), String>,
    ) -> Option<(AccountId, oidc::VerifiedTokens)>
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok((account_id, plan, profile)) = link(&tokens) else {
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "saved sign-in account lookup failed; check your connection and retry sign-in",
            );
            return None;
        };
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        if state.last_generation != 0 || state.pending.is_some() || !state.restore_allowed {
            return None;
        }
        if store_profile_material(vault, &account_id, &profile).is_err() {
            let _ = self.retire_unusable_vault_material(vault);
            state.restore_allowed = false;
            state.lease_expires_at = None;
            state.view = cleared_view(
                AccountSessionState::TerminalError,
                0,
                "credential storage is unavailable; retry sign-in",
            );
            return None;
        }
        state.restore_allowed = false;
        state.lease_expires_at = None;
        state.view = AccountView {
            state: AccountSessionState::Active,
            account_id: account_id.as_str().to_string(),
            plan_id: plan.as_str().to_string(),
            detail: "signed in".to_string(),
            request_generation: 0,
            display_name: profile.display_name,
            email: profile.email,
            photo_url: profile.photo_url,
        };
        Some((account_id, tokens))
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
        let (target, detail) = if cleanup_complete {
            (
                AccountSessionState::ReauthenticationRequired,
                "saved sign-in expired; sign in again",
            )
        } else {
            (
                AccountSessionState::TerminalError,
                "saved sign-in could not be retired; credential storage is unavailable; retry sign-in",
            )
        };
        // Unlike transient restore failures, an authoritative rejected grant
        // must never retain cached offline access in memory.
        state.lease_expires_at = None;
        state.view = cleared_view(target, 0, detail);
    }

    /// Retires an authoritatively rejected active session under the current
    /// generation fence. A successful cleanup leaves no restorable refresh or
    /// lease material; a partial cleanup keeps the durable rejection marker and
    /// fails the in-memory session closed instead of treating rejection like an
    /// offline network outage.
    fn retire_rejected_session<V>(&self, generation: u64, vault: &V)
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != generation
            || !matches!(
                state.view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        {
            return;
        }
        let cleanup_complete = self.retire_unusable_vault_material(vault);
        state.lease_expires_at = None;
        state.view = cleared_view(
            if cleanup_complete {
                AccountSessionState::ReauthenticationRequired
            } else {
                AccountSessionState::TerminalError
            },
            generation,
            if cleanup_complete {
                "sign-in expired; sign in again"
            } else {
                "sign-in expired but saved credentials could not be retired; retry sign-in"
            },
        );
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
        self.lease_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        // Persist the independent rejection fence before mutating vault state.
        // A crash during cleanup therefore cannot make the previously rejected
        // cached lease look authoritative again on the next process start.
        let marker_persisted = self.persist_rejected_restore_marker().is_ok();
        let lease_deleted = vault.delete(LEASE_VAULT_KEY).is_ok();
        let profile_deleted = vault.delete(PROFILE_VAULT_KEY).is_ok();
        let lease_retired = if lease_deleted {
            true
        } else {
            // The cached lease cannot authenticate without both its verified
            // key directory and device binding. If the lease entry itself
            // cannot be removed, retire either validation artifact instead.
            // Replacement fallbacks cover vault backends where update is
            // available even when deletion is not.
            let directory_retired = vault.delete(LEASE_DIRECTORY_VAULT_KEY).is_ok()
                || vault.store(LEASE_DIRECTORY_VAULT_KEY, b"[]").is_ok();
            if directory_retired || vault.delete(DEVICE_VAULT_KEY).is_ok() {
                true
            } else {
                let mut replacement = Zeroizing::new(vec![0_u8; 32]);
                getrandom::fill(replacement.as_mut_slice()).is_ok()
                    && vault
                        .store(DEVICE_VAULT_KEY, replacement.as_slice())
                        .is_ok()
            }
        };
        // When the independent marker could not be persisted, keep the rejected
        // refresh token unless the cached lease is already non-restorable. That
        // preserves the authoritative rejection evidence for a later retry.
        let refresh_deleted =
            (lease_retired || marker_persisted) && vault.delete(REFRESH_VAULT_KEY).is_ok();
        let authentication_cleanup = lease_deleted && refresh_deleted;
        let marker_cleared = if authentication_cleanup && marker_persisted {
            self.clear_rejected_restore_marker().is_ok()
        } else {
            !marker_persisted
        };
        authentication_cleanup && profile_deleted && marker_cleared
    }

    fn complete_restore_without_session(&self, target: AccountSessionState, detail: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation == 0 && state.pending.is_none() && state.restore_allowed {
            if state.view.state == AccountSessionState::OfflineLease
                && target == AccountSessionState::TerminalError
            {
                state.restore_allowed = false;
                state.view.detail = detail.to_string();
                return true;
            }
            state.restore_allowed = false;
            state.lease_expires_at = None;
            state.view = cleared_view(target, 0, detail);
            return true;
        }
        false
    }

    // Retry only repeatable discovery/link lookups. Replaying an ambiguous
    // refresh POST can revoke the rotating grant, so it must not use this path.
    fn retry_restore_lookup<T>(
        &self,
        mut lookup: impl FnMut() -> Result<T, String>,
    ) -> Result<T, String> {
        let mut attempt = 0;
        loop {
            if !self.state.lock().is_ok_and(|state| {
                state.last_generation == 0 && state.pending.is_none() && state.restore_allowed
            }) {
                return Err("saved sign-in restore was retired".to_string());
            }
            let result = lookup();
            if result.is_ok() || attempt == 2 {
                return result;
            }
            std::thread::sleep(Duration::from_millis(250 << attempt));
            attempt += 1;
        }
    }

    /// Returns whether a verified online or cached-offline session is installed.
    ///
    /// `None` means the account lifecycle state is momentarily busy. Callers
    /// that own an already-authoritative presentation snapshot may retain that
    /// snapshot for the current frame instead of rendering a false sign-out.
    /// Poisoned state remains an authoritative failure and returns `Some(false)`.
    #[must_use]
    pub fn try_is_authenticated(&self) -> Option<bool> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(_)) => return Some(false),
        };
        expire_lease_if_needed(&mut state, unix_now());
        Some(matches!(
            state.view.state,
            AccountSessionState::Active | AccountSessionState::OfflineLease
        ))
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
    pub fn oidc_endpoints(&self) -> Result<OidcEndpoints, String> {
        if let Ok(cached) = self.endpoints.lock()
            && let Some(endpoints) = cached.clone()
        {
            return Ok(endpoints);
        }
        let discovered = discover(&self.config.issuer)?;
        if let Ok(mut cached) = self.endpoints.lock() {
            *cached = Some(discovered.clone());
        }
        Ok(discovered)
    }

    /// Starts one generation-fenced login transaction and returns the
    /// browser authorization address for the desktop to open.
    ///
    /// # Errors
    ///
    /// Returns a redacted actionable error when a transaction is already
    /// pending, the generation is retired, the control plane is
    /// unconfigured, or the loopback listener cannot bind.
    pub fn begin_login(&self, request_generation: u64) -> Result<LoginAuthorization, String> {
        if !self.config.is_configured() {
            self.transition_to(
                AccountSessionState::TerminalError,
                None,
                None,
                "sign-in is unavailable; the account service is not configured",
            );
            return Err(
                "sign-in is unavailable; the account service is not configured".to_string(),
            );
        }
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
        let listener = LoopbackListener::bind()?;
        let redirect_uri = listener.redirect_uri();
        let endpoints = self.oidc_endpoints()?;
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
        state.lease_expires_at = None;
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
            .name("axiusflow-account-login".to_string())
            .spawn(move || service.run_login_transaction(request_generation, &listener))
            .is_err()
        {
            state.pending = None;
            state.lease_expires_at = None;
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
                state.lease_expires_at = None;
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
        expire_lease_if_needed(&mut state, unix_now());
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
            .name("axiusflow-account-profile-refresh".to_string())
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

    /// Signs out the shared session, deleting vault refresh and lease material.
    ///
    /// Local deletion happens synchronously so a concurrent login cannot
    /// observe stale material; server-side revocation follows on a bounded
    /// worker with the in-memory refresh copy and never blocks the coordinator.
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
        let token = vault
            .load(REFRESH_VAULT_KEY)
            .unwrap_or_default()
            .filter(|token| !token.is_empty());
        // Retire every background completion before deleting credentials.
        // Workers take the same state lock while committing vault writes.
        if let Ok(mut state) = self.state.lock() {
            let generation = state.last_generation;
            state.restore_allowed = false;
            state.pending = None;
            state.lease_expires_at = None;
            state.view = cleared_view(AccountSessionState::SignedOut, generation, "signed out");
        }
        let lease_deleted = vault.delete(LEASE_VAULT_KEY).is_ok();
        let refresh_deleted = vault.delete(REFRESH_VAULT_KEY).is_ok();
        let profile_deleted = vault.delete(PROFILE_VAULT_KEY).is_ok();
        // Revocation reuses already-cached endpoints only: sign-out never
        // performs discovery on the coordinator path.
        let cached = self
            .endpoints
            .lock()
            .map(|cached| cached.clone())
            .unwrap_or_default();
        let revocation = cached.zip(token.and_then(|token| String::from_utf8(token).ok()));
        let deleted = lease_deleted && refresh_deleted && profile_deleted;
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
            state.lease_expires_at = None;
            state.view.clone()
        };
        if let Some((endpoints, token)) = revocation {
            std::thread::Builder::new()
                .name("axiusflow-account-revoke".to_string())
                .spawn(move || {
                    if oidc::revoke_refresh(&endpoints, &token).is_err() {
                        eprintln!("Axiusflow sign-out revocation degraded");
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
    /// connection stays open while the engine exchanges, links, and stores;
    /// the browser then reads the completed outcome, never mere receipt.
    /// Processing is bounded by the endpoint timeouts plus local vault
    /// work, and every path answers the waiting browser exactly once.
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
        match self.exchange_and_link(generation, &code) {
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
        state.view.detail = "exchanging authorization code".to_string();
        Ok(validated.code)
    }

    /// Exchanges, links, and stores one transaction. Returns whether the
    /// Active session published: `Ok(true)` completes the sign-in,
    /// `Ok(false)` leaves an already-recorded terminal failure (vault
    /// failure) or a retired generation untouched, and `Err` carries the
    /// redacted detail for the browser page and the desktop account view.
    fn exchange_and_link(&self, generation: u64, code: &str) -> Result<bool, String> {
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
                pending.verifier.encoded().to_string(),
                pending.nonce.clone(),
            )
        };
        let outcome = self.oidc_endpoints().and_then(|endpoints| {
            // One connection pool for the whole transaction: exchange, JWKS
            // verification, and linking all reuse it instead of paying a
            // fresh TLS handshake per stage.
            let agent = oidc_agent();
            exchange_code(
                &endpoints,
                &agent,
                &self.config.client_id,
                &redirect_uri,
                code,
                &verifier,
                &nonce,
            )
            .and_then(|tokens| {
                link_subject(&endpoints, &agent, &tokens.id_token, &tokens.subject)
                    .map(|(account, plan, profile)| (account, plan, profile, tokens, agent))
            })
        });
        match outcome {
            Ok((account_id, plan, profile, tokens, agent)) => {
                let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
                    self.fail_generation(
                        generation,
                        "credential storage is unavailable; retry sign-in",
                    );
                    return Ok(false);
                };
                let completed = self.complete_with_tokens(
                    generation,
                    &account_id,
                    plan,
                    &profile,
                    tokens.refresh.as_deref(),
                    &vault,
                );
                if completed {
                    // Lease warmup leaves the browser-response path: Active
                    // is already published, and generation-fenced
                    // apply_lease_outcome keeps a late warmup from touching
                    // retired state or weakening enforcement.
                    let service = self.clone();
                    std::thread::Builder::new()
                        .name("axiusflow-account-lease-warmup".to_string())
                        .spawn(move || {
                            if let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) {
                                service.refresh_lease_once(
                                    generation,
                                    &tokens,
                                    &account_id,
                                    &agent,
                                    &vault,
                                );
                            }
                        })
                        .ok();
                }
                Ok(completed)
            }
            Err(detail) => Err(detail),
        }
    }

    /// Publishes the Active session only after the refresh material is
    /// stored: exchange, link, and vault all precede any success the
    /// desktop can render. A retired generation assigns nothing, so a late
    /// callback can never restore another user's profile. Returns whether
    /// the Active session published.
    fn complete_with_tokens<V>(
        &self,
        generation: u64,
        account_id: &AccountId,
        plan: PlanId,
        profile: &AccountProfile,
        refresh_token: Option<&str>,
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
        let stored = store_refresh_material(vault, refresh_token).is_ok()
            && store_profile_material(vault, account_id, profile).is_ok();
        if !stored {
            let _ = self.retire_unusable_vault_material(vault);
        }
        let marker_cleared = stored && self.clear_rejected_restore_marker().is_ok();
        if !marker_cleared {
            let generation = state.last_generation;
            state.pending = None;
            state.lease_expires_at = None;
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
        let generation = state.last_generation;
        state.pending = None;
        state.lease_expires_at = None;
        state.view = AccountView {
            state: AccountSessionState::Active,
            account_id: account_id.as_str().to_string(),
            plan_id: plan.as_str().to_string(),
            detail: "signed in".to_string(),
            request_generation: generation,
            display_name: profile.display_name.clone(),
            email: profile.email.clone(),
            photo_url: profile.photo_url.clone(),
        };
        let service = self.clone();
        if std::thread::Builder::new()
            .name("axiusflow-account-lease".to_string())
            .spawn(move || service.run_lease_worker(generation))
            .is_err()
        {
            state.lease_expires_at = None;
            state.view = cleared_view(
                AccountSessionState::TerminalError,
                generation,
                "account entitlement worker could not start; retry sign-in",
            );
            return false;
        }
        true
    }

    /// Returns whether one generation still owns the shared session.
    fn is_current(&self, generation: u64) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.last_generation == generation
                && (state.view.state == AccountSessionState::Active
                    || state.view.state == AccountSessionState::OfflineLease)
        })
    }

    fn run_lease_worker(&self, generation: u64) {
        loop {
            if !sleep_until_lease_round(self, generation) {
                return;
            }
            if !self.is_current(generation) {
                return;
            }
            self.lease_round(generation);
        }
    }

    fn spawn_lease_worker(&self, generation: u64) -> bool {
        let service = self.clone();
        std::thread::Builder::new()
            .name("axiusflow-account-lease".to_string())
            .spawn(move || service.run_lease_worker(generation))
            .is_ok()
    }

    fn fail_lease_worker_start(&self, generation: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != generation
            || !matches!(
                state.view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        {
            return;
        }
        state.lease_expires_at = None;
        state.view = cleared_view(
            AccountSessionState::TerminalError,
            generation,
            "account entitlement worker could not start; retry sign-in",
        );
    }

    fn lease_round(&self, generation: u64) {
        let Ok(_refresh_guard) = self.refresh_gate.lock() else {
            note_lease("refresh-gate-unavailable");
            return;
        };
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            note_lease("vault-unavailable");
            return;
        };
        let outcome = refresh_lease_round(self, generation, &vault);
        self.apply_lease_outcome(generation, outcome);
    }

    /// Best-effort initial lease right after sign-in.
    fn refresh_lease_once<V>(
        &self,
        generation: u64,
        tokens: &oidc::VerifiedTokens,
        account_id: &AccountId,
        agent: &ureq::Agent,
        vault: &V,
    ) where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        let Ok(_refresh_guard) = self.refresh_gate.lock() else {
            return;
        };
        let outcome = initial_lease_round(self, generation, tokens, account_id, agent, vault);
        self.apply_lease_outcome(generation, outcome);
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
        // Reuse the same verified refresh/link path as the scheduled lease
        // worker. Profile fields are generation/account fenced, while plan
        // changes remain lease-authoritative. Apply only verified online lease
        // outcomes here: a cosmetic refresh must not downgrade a healthy
        // session because of transient control-plane unavailability. Terminal
        // rejection/storage failures retire state inside the shared path.
        let outcome = refresh_lease_round(self, generation, &vault);
        self.apply_profile_refresh_outcome(generation, outcome);
    }

    fn apply_linked_profile<V>(
        &self,
        vault: &V,
        generation: u64,
        expected_account: &AccountId,
        linked_account: &AccountId,
        profile: AccountProfile,
    ) -> bool
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        if linked_account != expected_account {
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation != generation
            || state.view.account_id != expected_account.as_str()
            || !matches!(
                state.view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        {
            return false;
        }
        if store_profile_material(vault, expected_account, &profile).is_err() {
            return false;
        }
        state.view.display_name = profile.display_name;
        state.view.email = profile.email;
        state.view.photo_url = profile.photo_url;
        true
    }

    fn apply_lease_outcome(&self, generation: u64, outcome: lease::RefreshOutcome) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != generation {
            return;
        }
        let current = state.view.state;
        let active = AccountSessionState::Active;
        let offline = AccountSessionState::OfflineLease;
        if current != active && current != offline {
            return;
        }
        match outcome {
            lease::RefreshOutcome::Refreshed(plan) | lease::RefreshOutcome::Current(Some(plan)) => {
                state.view.plan_id = plan.as_str().to_string();
                if current != active {
                    state.view.state = active;
                    state.view.detail = "signed in".to_string();
                }
                note_lease("valid");
            }
            lease::RefreshOutcome::Current(None) => note_lease("valid"),
            lease::RefreshOutcome::Deferred => note_lease("deferred"),
            lease::RefreshOutcome::OfflineCovered(plan) => {
                state.view.plan_id = plan.as_str().to_string();
                if current != offline {
                    state.view.state = offline;
                    state.view.detail =
                        "signed in with a cached subscription; reconnect to refresh".to_string();
                }
                note_lease("offline-covered");
            }
            lease::RefreshOutcome::Unavailable => {
                state.lease_expires_at = None;
                state.view.state = AccountSessionState::ReauthenticationRequired;
                state.view.detail = "sign-in expired; sign in again".to_string();
                note_lease("unavailable");
            }
        }
    }

    fn apply_profile_refresh_outcome(&self, generation: u64, outcome: lease::RefreshOutcome) {
        if matches!(
            outcome,
            lease::RefreshOutcome::Refreshed(_) | lease::RefreshOutcome::Current(Some(_))
        ) {
            self.apply_lease_outcome(generation, outcome);
        }
    }

    fn record_lease_expiry(&self, generation: u64, expires_at: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.last_generation != generation
            || !matches!(
                state.view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        {
            return false;
        }
        state.lease_expires_at = Some(expires_at);
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
        state.lease_expires_at = None;
        if detail.contains("timed out") || detail.contains("cancelled") {
            state.view = cleared_view(AccountSessionState::SignedOut, generation, detail);
        } else {
            state.view = cleared_view(AccountSessionState::TerminalError, generation, detail);
        }
    }

    fn transition_to(
        &self,
        state_value: AccountSessionState,
        account_id: Option<&AccountId>,
        plan: Option<PlanId>,
        detail: &str,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let mut view = cleared_view(state_value, state.last_generation, detail);
        view.account_id = account_id
            .map(AccountId::as_str)
            .unwrap_or_default()
            .to_string();
        view.plan_id = plan.map(PlanId::as_str).unwrap_or_default().to_string();
        state.lease_expires_at = None;
        state.view = view;
    }
}

fn claim_profile_refresh(view: &AccountView, in_flight: &AtomicBool) -> Option<u64> {
    if !matches!(
        view.state,
        AccountSessionState::Active | AccountSessionState::OfflineLease
    ) {
        return None;
    }
    in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| view.request_generation)
}

/// One sanitized view with identity, plan, and profile cleared. Every
/// non-Active assignment flows through here so sign-out, cancel, expiry,
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

fn expire_lease_if_needed(state: &mut ServiceState, now_unix_seconds: u64) {
    let missing_offline_deadline =
        state.view.state == AccountSessionState::OfflineLease && state.lease_expires_at.is_none();
    let expired_deadline = state
        .lease_expires_at
        .is_some_and(|expires_at| now_unix_seconds >= expires_at);
    if missing_offline_deadline
        || (expired_deadline
            && matches!(
                state.view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            ))
    {
        let generation = state.last_generation;
        state.lease_expires_at = None;
        state.view = cleared_view(
            AccountSessionState::ReauthenticationRequired,
            generation,
            "sign-in expired; sign in again",
        );
    }
}

fn lock_state(
    state: &Arc<Mutex<ServiceState>>,
) -> Result<std::sync::MutexGuard<'_, ServiceState>, String> {
    state
        .lock()
        .map_err(|_| "account state is unavailable".to_string())
}

/// Redacted lease observation: outcome class only, never identities.
fn note_lease(outcome: &str) {
    eprintln!("Axiusflow lease: {outcome}");
}

/// Sleeps until the next lease round in interruptible slices. Returns false
/// when the generation retired while waiting.
fn sleep_until_lease_round(service: &AccountService, generation: u64) -> bool {
    let mut jitter = [0_u8; 8];
    let jitter = if getrandom::fill(&mut jitter).is_ok() {
        Some(jitter)
    } else {
        None
    };
    let wait = Duration::from_secs(lease_refresh_wait_seconds(jitter));
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !service.is_current(generation) {
            return false;
        }
        std::thread::sleep(Duration::from_secs(60).min(deadline - Instant::now()));
    }
    service.is_current(generation)
}

fn lease_refresh_wait_seconds(jitter: Option<[u8; 8]>) -> u64 {
    LEASE_REFRESH_INTERVAL_SECONDS + jitter.map_or(0, |bytes| u64::from_le_bytes(bytes) % 1800)
}

struct LeaseSession {
    account_id: AccountId,
    device_id: String,
    refresh_token: String,
    endpoints: OidcEndpoints,
    origin: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaseSessionError {
    Unavailable,
    Storage,
}

fn lease_session<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
) -> Result<LeaseSession, LeaseSessionError>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    // Resolve network-owned endpoint metadata before taking the lifecycle lock.
    // Credential selection below must then remain under one generation fence:
    // otherwise a stale worker can pass the generation check, a newer login can
    // replace the durable refresh token, and the stale worker can consume that
    // newer rotating token before any later fence observes the replacement.
    let endpoints = service
        .oidc_endpoints()
        .map_err(|_| LeaseSessionError::Unavailable)?;
    let origin =
        control_plane_origin(&endpoints.issuer).map_err(|_| LeaseSessionError::Unavailable)?;
    let state = service
        .state
        .lock()
        .map_err(|_| LeaseSessionError::Unavailable)?;
    if state.last_generation != generation {
        return Err(LeaseSessionError::Unavailable);
    }
    let account_id =
        AccountId::try_new(&state.view.account_id).map_err(|_| LeaseSessionError::Unavailable)?;
    let refresh_token = vault
        .load(REFRESH_VAULT_KEY)
        .map_err(|_| LeaseSessionError::Storage)?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|token| !token.is_empty())
        .ok_or(LeaseSessionError::Storage)?;
    let mut device_key = vault
        .load(DEVICE_VAULT_KEY)
        .map_err(|_| LeaseSessionError::Storage)?
        .filter(|key| !key.is_empty());
    if device_key.is_none() {
        ensure_device_key(vault).map_err(|_| LeaseSessionError::Storage)?;
        device_key = vault
            .load(DEVICE_VAULT_KEY)
            .map_err(|_| LeaseSessionError::Storage)?
            .filter(|key| !key.is_empty());
    }
    let device_id = device_key
        .map(|key| device_id_for_key(&key))
        .ok_or(LeaseSessionError::Storage)?;
    Ok(LeaseSession {
        account_id,
        device_id,
        refresh_token,
        endpoints,
        origin,
    })
}

fn persist_rotated_refresh<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
    refresh_token: Option<&str>,
) -> bool
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Some(refresh_token) = refresh_token.filter(|token| !token.is_empty()) else {
        return service.is_current(generation);
    };
    let secret = Zeroizing::new(refresh_token.as_bytes().to_vec());
    let Ok(mut state) = service.state.lock() else {
        return false;
    };
    if state.last_generation != generation
        || !matches!(
            state.view.state,
            AccountSessionState::Active | AccountSessionState::OfflineLease
        )
    {
        return false;
    }
    if vault.store(REFRESH_VAULT_KEY, secret.as_slice()).is_err() {
        // A successful rotating grant may have invalidated the durable token
        // we still hold. Retire every restorable artifact under this same
        // generation fence so a restart cannot resurrect the now-unusable
        // saved session. Partial cleanup is fenced by the existing durable
        // rejection marker.
        let _ = service.retire_unusable_vault_material(vault);
        state.restore_allowed = false;
        state.lease_expires_at = None;
        state.view = cleared_view(
            AccountSessionState::TerminalError,
            generation,
            "credential storage is unavailable; retry sign-in",
        );
        return false;
    }
    true
}

fn accept_refresh_grant<V>(
    service: &AccountService,
    generation: u64,
    session: &LeaseSession,
    vault: &V,
    outcome: Result<oidc::VerifiedTokens, oidc::RefreshGrantError>,
    now_unix_seconds: u64,
) -> Result<oidc::VerifiedTokens, lease::RefreshOutcome>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    match outcome {
        Ok(tokens) => {
            if persist_rotated_refresh(service, generation, vault, tokens.refresh.as_deref()) {
                Ok(tokens)
            } else {
                Err(lease::RefreshOutcome::Unavailable)
            }
        }
        Err(oidc::RefreshGrantError::Rejected) => {
            note_lease("refresh-rejected");
            service.retire_rejected_session(generation, vault);
            Err(lease::RefreshOutcome::Unavailable)
        }
        Err(oidc::RefreshGrantError::Unavailable) => Err(cached_outcome(
            service,
            generation,
            session,
            vault,
            now_unix_seconds,
        )),
    }
}

fn persist_lease_directory<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
    fetched_keys: &[LeaseKey],
) -> bool
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Ok(encoded) = serde_json::to_vec(fetched_keys) else {
        fail_current_storage(service, generation);
        return false;
    };
    let Ok(mut state) = service.state.lock() else {
        return false;
    };
    if state.last_generation != generation
        || !matches!(
            state.view.state,
            AccountSessionState::Active | AccountSessionState::OfflineLease
        )
    {
        return false;
    }
    // The fetched directory is the authority that validated this online
    // lease. Commit it under the session-generation fence before any lease
    // validated by it can become durable. Re-store even when the in-memory
    // cache matches: memory equality does not prove the durable copy exists.
    if vault.store(LEASE_DIRECTORY_VAULT_KEY, &encoded).is_err() {
        note_lease("directory-storage-unavailable");
        fail_locked_durable_storage(service, &mut state, generation, vault);
        return false;
    }
    let Ok(mut cached) = service.lease_keys.lock() else {
        fail_locked_durable_storage(service, &mut state, generation, vault);
        return false;
    };
    // Keep the generation fence through the in-memory authority update. A new
    // login cannot advance ownership between the durable directory commit and
    // this cache replacement.
    cached.clear();
    cached.extend_from_slice(fetched_keys);
    true
}

fn fail_current_storage(service: &AccountService, generation: u64) {
    let Ok(mut state) = service.state.lock() else {
        return;
    };
    if state.last_generation != generation
        || !matches!(
            state.view.state,
            AccountSessionState::Active | AccountSessionState::OfflineLease
        )
    {
        return;
    }
    state.restore_allowed = false;
    state.lease_expires_at = None;
    state.view = cleared_view(
        AccountSessionState::TerminalError,
        generation,
        "credential storage is unavailable; retry sign-in",
    );
}

fn fail_locked_durable_storage<V>(
    service: &AccountService,
    state: &mut ServiceState,
    generation: u64,
    vault: &V,
) where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    if state.last_generation != generation
        || !matches!(
            state.view.state,
            AccountSessionState::Active | AccountSessionState::OfflineLease
        )
    {
        return;
    }
    // A verified online entitlement could encode a downgrade or revocation.
    // If its durable commit fails, the previous cached entitlement must not be
    // able to reappear after restart. Retire every restorable artifact while
    // the session-generation fence is still held; the independent marker
    // covers partial vault cleanup.
    let _ = service.retire_unusable_vault_material(vault);
    state.restore_allowed = false;
    state.lease_expires_at = None;
    state.view = cleared_view(
        AccountSessionState::TerminalError,
        generation,
        "credential storage is unavailable; retry sign-in",
    );
}

fn validate_and_cache<V>(
    service: &AccountService,
    generation: u64,
    session: &LeaseSession,
    agent: &ureq::Agent,
    compact: &str,
    vault: &V,
    now_unix_seconds: u64,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let fetched_keys = lease::fetch_directory(agent, &session.origin);
    match fetched_keys.as_deref() {
        Ok(keys) => validate_and_cache_after_directory_fetch(
            service,
            generation,
            session,
            compact,
            vault,
            now_unix_seconds,
            Ok(keys),
        ),
        Err(_) => validate_and_cache_after_directory_fetch(
            service,
            generation,
            session,
            compact,
            vault,
            now_unix_seconds,
            Err(()),
        ),
    }
}

fn validate_and_cache_after_directory_fetch<V>(
    service: &AccountService,
    generation: u64,
    session: &LeaseSession,
    compact: &str,
    vault: &V,
    now_unix_seconds: u64,
    directory: Result<&[LeaseKey], ()>,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let (fetched_keys, directory_unavailable) = match directory {
        Ok(keys) => (keys, false),
        Err(()) => (&[][..], true),
    };
    let outcome = validate_and_cache_with_directory(
        service,
        generation,
        session,
        compact,
        vault,
        now_unix_seconds,
        fetched_keys,
    );
    if directory_unavailable
        && outcome == lease::RefreshOutcome::Unavailable
        && service.is_current(generation)
    {
        // Directory reachability is control-plane availability, not an
        // authoritative entitlement rejection. If a freshly returned lease
        // needs an unknown rotated key while the directory is unavailable,
        // keep a still-valid cached lease as the bounded offline authority.
        return cached_outcome(service, generation, session, vault, now_unix_seconds);
    }
    outcome
}

fn transition_lease_keys(
    session: &LeaseSession,
    cached_compact: Option<&str>,
    cached_keys: &[LeaseKey],
    fetched_keys: &[LeaseKey],
    cached_is_authoritative: bool,
    now_unix_seconds: u64,
) -> Option<Vec<LeaseKey>> {
    let mut transition_keys = fetched_keys.to_owned();
    if fetched_keys.is_empty() || !cached_is_authoritative {
        return Some(transition_keys);
    }
    let cached = cached_compact?;
    if lease::validate_compact(
        cached,
        fetched_keys,
        &session.account_id,
        &session.device_id,
        now_unix_seconds,
    )
    .is_ok()
    {
        return Some(transition_keys);
    }
    let old_key = cached_keys.iter().find(|key| {
        lease::validate_compact(
            cached,
            std::slice::from_ref(*key),
            &session.account_id,
            &session.device_id,
            now_unix_seconds,
        )
        .is_ok()
    })?;
    if let Some(replacement) = transition_keys.iter().find(|key| key.0 == old_key.0) {
        // Compact leases resolve keys by `kid`, so different public keys with
        // one identifier cannot coexist during the two-write transition.
        if replacement.1 != old_key.1 {
            note_lease("directory-key-id-conflict");
            return None;
        }
    } else {
        transition_keys.push(old_key.clone());
    }
    Some(transition_keys)
}

fn validate_and_cache_with_directory<V>(
    service: &AccountService,
    generation: u64,
    session: &LeaseSession,
    compact: &str,
    vault: &V,
    now_unix_seconds: u64,
    fetched_keys: &[LeaseKey],
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Ok(cached_compact) = lease::load_cached(vault, LEASE_VAULT_KEY) else {
        fail_current_storage(service, generation);
        return lease::RefreshOutcome::Unavailable;
    };
    let (claims, cached_claims, transition_keys) = {
        let cached_keys = service
            .lease_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let validation_keys = if fetched_keys.is_empty() {
            cached_keys.as_slice()
        } else {
            fetched_keys
        };
        let Ok(claims) = lease::validate_compact(
            compact,
            validation_keys,
            &session.account_id,
            &session.device_id,
            now_unix_seconds,
        ) else {
            return lease::RefreshOutcome::Unavailable;
        };
        // Compare revisions against the durable lease using the authority that
        // made that lease restorable before this refresh. A signing-key
        // rotation must not let a lower-revision lease bypass monotonic cache
        // protection merely because the newly fetched directory no longer
        // contains the old signing key.
        let cached_claims = cached_compact.as_deref().and_then(|cached| {
            lease::validate_compact(
                cached,
                cached_keys.as_slice(),
                &session.account_id,
                &session.device_id,
                now_unix_seconds,
            )
            .ok()
        });
        // Make the directory write compatible with both sides of the durable
        // lease replacement. If rotation removed the key for the still-valid
        // cached lease, retain only the old key that verifies that lease. A
        // crash before the following lease write then leaves a valid old pair;
        // a crash after it leaves a directory that also contains every fetched
        // key needed by the new lease. The next successful refresh naturally
        // drops this compatibility key once the durable lease uses a fetched
        // key.
        let Some(transition_keys) = transition_lease_keys(
            session,
            cached_compact.as_deref(),
            cached_keys.as_slice(),
            fetched_keys,
            cached_claims.is_some(),
            now_unix_seconds,
        ) else {
            return lease::RefreshOutcome::Unavailable;
        };
        (claims, cached_claims, transition_keys)
    };
    // Persist a rotated directory only after it validates a lease from the
    // same response path. A malformed lease cannot poison offline restart.
    if !fetched_keys.is_empty()
        && !persist_lease_directory(service, generation, vault, &transition_keys)
    {
        return lease::RefreshOutcome::Unavailable;
    }
    match cached_claims {
        Some(cached) if cached.revision() >= claims.revision() => {
            let _ = service.record_lease_expiry(generation, cached.expires_at());
            lease::RefreshOutcome::Current(Some(cached.plan()))
        }
        _ => {
            let secret = Zeroizing::new(compact.as_bytes().to_vec());
            let Ok(mut state) = service.state.lock() else {
                return lease::RefreshOutcome::Unavailable;
            };
            if state.last_generation != generation
                || !matches!(
                    state.view.state,
                    AccountSessionState::Active | AccountSessionState::OfflineLease
                )
            {
                return lease::RefreshOutcome::Unavailable;
            }
            if vault.store(LEASE_VAULT_KEY, secret.as_slice()).is_err() {
                note_lease("lease-storage-unavailable");
                fail_locked_durable_storage(service, &mut state, generation, vault);
                return lease::RefreshOutcome::Unavailable;
            }
            state.lease_expires_at = Some(claims.expires_at());
            lease::RefreshOutcome::Refreshed(claims.plan())
        }
    }
}

fn cached_lease_covers<V>(
    service: &AccountService,
    generation: u64,
    session: &LeaseSession,
    vault: &V,
    now_unix_seconds: u64,
) -> Option<PlanId>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    // Offline coverage re-validates the cached lease against the last-known
    // directory without touching the network, and additionally requires the
    // remaining validity to sit inside the approved offline window.
    let cached = match lease::load_cached(vault, LEASE_VAULT_KEY) {
        Ok(Some(cached)) => cached,
        Ok(None) => return None,
        Err(_) => {
            fail_current_storage(service, generation);
            return None;
        }
    };
    let claims = {
        let keys = service
            .lease_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lease::validate_compact(
            &cached,
            &keys,
            &session.account_id,
            &session.device_id,
            now_unix_seconds,
        )
        .ok()
        .filter(|claims| {
            claims.expires_at().saturating_sub(now_unix_seconds) <= LEASE_OFFLINE_VALIDITY_SECONDS
        })?
    };
    let _ = service.record_lease_expiry(generation, claims.expires_at());
    Some(claims.plan())
}

/// One background lease round: refresh grant, lease fetch, monotonic cache.
fn refresh_lease_round<V>(
    service: &AccountService,
    generation: u64,
    vault: &V,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let session = match lease_session(service, generation, vault) {
        Ok(session) => session,
        Err(LeaseSessionError::Storage) => {
            fail_current_storage(service, generation);
            return lease::RefreshOutcome::Unavailable;
        }
        Err(LeaseSessionError::Unavailable) => return lease::RefreshOutcome::Unavailable,
    };
    let now = unix_now();
    // One connection pool per background round: grant, link, lease, and
    // directory reuse it instead of paying a fresh handshake per stage.
    let agent = oidc_agent();
    let grant_permit = match service.claim_refresh_grant() {
        Ok(Some(permit)) => permit,
        Ok(None) => return lease::RefreshOutcome::Deferred,
        Err(_) => return lease::RefreshOutcome::Unavailable,
    };
    let refresh = refresh_grant(
        &session.endpoints,
        &agent,
        &service.config.client_id,
        &session.refresh_token,
    );
    let tokens = match accept_refresh_grant(service, generation, &session, vault, refresh, now) {
        Ok(tokens) => tokens,
        Err(outcome) => return outcome,
    };
    drop(grant_permit);
    if let Ok((account_id, _plan, profile)) = link_subject(
        &session.endpoints,
        &agent,
        &tokens.id_token,
        &tokens.subject,
    ) {
        let _ = service.apply_linked_profile(
            vault,
            generation,
            &session.account_id,
            &account_id,
            profile,
        );
    }
    let Ok(compact) = lease::fetch_compact(
        &session.endpoints,
        &agent,
        &tokens.id_token,
        &tokens.subject,
        &session.device_id,
    ) else {
        return cached_outcome(service, generation, &session, vault, now);
    };
    validate_and_cache(service, generation, &session, &agent, &compact, vault, now)
}

fn cached_outcome<V>(
    service: &AccountService,
    generation: u64,
    session: &LeaseSession,
    vault: &V,
    now_unix_seconds: u64,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    if let Some(plan) = cached_lease_covers(service, generation, session, vault, now_unix_seconds) {
        lease::RefreshOutcome::OfflineCovered(plan)
    } else {
        lease::RefreshOutcome::Unavailable
    }
}

/// Best-effort initial lease with login-time tokens (no grant needed).
fn initial_lease_round<V>(
    service: &AccountService,
    generation: u64,
    tokens: &oidc::VerifiedTokens,
    account_id: &AccountId,
    agent: &ureq::Agent,
    vault: &V,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let session = match lease_session(service, generation, vault) {
        Ok(session) => session,
        Err(LeaseSessionError::Storage) => {
            fail_current_storage(service, generation);
            return lease::RefreshOutcome::Unavailable;
        }
        Err(LeaseSessionError::Unavailable) => return lease::RefreshOutcome::Unavailable,
    };
    if &session.account_id != account_id {
        return lease::RefreshOutcome::Unavailable;
    }
    let Ok(compact) = lease::fetch_compact(
        &session.endpoints,
        agent,
        &tokens.id_token,
        &tokens.subject,
        &session.device_id,
    ) else {
        return lease::RefreshOutcome::Current(None);
    };
    // A failed initial fetch leaves the fresh Active session untouched.
    match validate_and_cache(
        service,
        generation,
        &session,
        agent,
        &compact,
        vault,
        unix_now(),
    ) {
        lease::RefreshOutcome::Unavailable => lease::RefreshOutcome::Current(None),
        outcome => outcome,
    }
}

/// Credential vault that is always unavailable, used when native storage
/// cannot be constructed so sign-out still clears engine state.
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

fn store_refresh_material<V>(vault: &V, refresh_token: Option<&str>) -> Result<(), String>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    ensure_device_key(vault)?;
    if let Some(refresh) = refresh_token.filter(|token| !token.is_empty()) {
        let secret = Zeroizing::new(refresh.as_bytes().to_vec());
        vault
            .store(REFRESH_VAULT_KEY, secret.as_slice())
            .map_err(|_| "credential storage is unavailable".to_string())?;
    }
    Ok(())
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

fn load_profile_material<V>(
    vault: &V,
    expected_account: &AccountId,
) -> Result<Option<AccountProfile>, ()>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let Some(encoded) = vault.load(PROFILE_VAULT_KEY).map_err(|_| ())? else {
        return Ok(None);
    };
    if encoded.is_empty() || encoded.len() > MAXIMUM_PROFILE_VAULT_BYTES {
        return Ok(None);
    }
    let Ok(cached) = serde_json::from_slice::<CachedAccountProfile>(&encoded) else {
        return Ok(None);
    };
    if cached.schema_version != PROFILE_VAULT_SCHEMA_VERSION
        || cached.account_id != expected_account.as_str()
        || cached.display_name.len() > MAXIMUM_PROFILE_NAME_BYTES
        || cached.email.len() > MAXIMUM_PROFILE_EMAIL_BYTES
        || cached.photo_url.len() > MAXIMUM_PROFILE_PHOTO_URL_BYTES
    {
        return Ok(None);
    }
    Ok(Some(AccountProfile {
        display_name: cached.display_name,
        email: cached.email,
        photo_url: cached.photo_url,
    }))
}

fn ensure_device_key<V>(vault: &V) -> Result<(), String>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    if vault
        .load(DEVICE_VAULT_KEY)
        .map_err(|_| "credential storage is unavailable".to_string())?
        .is_some()
    {
        return Ok(());
    }
    let mut key = Zeroizing::new(vec![0_u8; 32]);
    getrandom::fill(key.as_mut_slice()).map_err(|_| "system CSPRNG unavailable".to_string())?;
    vault
        .store(DEVICE_VAULT_KEY, key.as_slice())
        .map_err(|_| "credential storage is unavailable".to_string())
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
mod tests {
    use super::{
        AccountRestoreReadiness, AccountService, AccountServiceConfig, DEVICE_VAULT_KEY,
        LEASE_DIRECTORY_VAULT_KEY, LEASE_VAULT_KEY, LOGIN_TIMEOUT, LocalRestore, REFRESH_VAULT_KEY,
        RejectedRestoreMarker, UnavailableVault, claim_profile_refresh,
        oidc::{self, AccountProfile, VerifiedTokens},
    };
    use axiusflow_account::{AccountId, PlanId};
    use axiusflow_contracts::{AccountSessionState, AccountView};
    use axiusflow_platform_runtime::CredentialVault;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::{
        collections::HashMap,
        fs,
        net::TcpListener,
        path::PathBuf,
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::Duration,
    };

    static MARKER_FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct MemoryVault {
        entries: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl CredentialVault for MemoryVault {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            self.entries
                .lock()
                .map_err(|_| "lock".to_string())?
                .insert(key.to_string(), secret.to_vec());
            Ok(())
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok(self
                .entries
                .lock()
                .map_err(|_| "lock".to_string())?
                .get(key)
                .cloned())
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            self.entries
                .lock()
                .map_err(|_| "lock".to_string())?
                .remove(key);
            Ok(())
        }
    }

    struct SelectiveLoadFailureVault<'a> {
        inner: &'a MemoryVault,
        failing_key: &'static str,
    }

    impl CredentialVault for SelectiveLoadFailureVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            if key == self.failing_key {
                return Err("selected load failure".to_string());
            }
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            self.inner.delete(key)
        }
    }

    struct GenerationRaceVault<'a> {
        inner: &'a MemoryVault,
        state: Arc<Mutex<super::ServiceState>>,
        attempted_advance: AtomicBool,
        advanced: AtomicBool,
        replacement_generation: u64,
    }

    impl CredentialVault for GenerationRaceVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            if key == REFRESH_VAULT_KEY {
                self.attempted_advance.store(true, Ordering::Release);
                if let Ok(mut state) = self.state.try_lock() {
                    state.last_generation = self.replacement_generation;
                    state.view.request_generation = self.replacement_generation;
                    self.inner.store(REFRESH_VAULT_KEY, b"newer-refresh")?;
                    self.advanced.store(true, Ordering::Release);
                }
            }
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            self.inner.delete(key)
        }
    }

    struct SelectiveStoreFailureVault<'a> {
        inner: &'a MemoryVault,
        failing_key: &'static str,
    }

    impl CredentialVault for SelectiveStoreFailureVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            if key == self.failing_key {
                return Err("selected store failure".to_string());
            }
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            self.inner.delete(key)
        }
    }

    struct SelectiveDeleteFailureVault<'a> {
        inner: &'a MemoryVault,
        failing_key: &'static str,
    }

    impl CredentialVault for SelectiveDeleteFailureVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            if key == self.failing_key {
                return Err("selected delete failure".to_string());
            }
            self.inner.delete(key)
        }
    }

    struct LeaseArtifactDeleteFailureVault<'a> {
        inner: &'a MemoryVault,
    }

    impl CredentialVault for LeaseArtifactDeleteFailureVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            if matches!(key, LEASE_VAULT_KEY | LEASE_DIRECTORY_VAULT_KEY) {
                return Err("lease artifact delete failure".to_string());
            }
            self.inner.delete(key)
        }
    }

    struct LeaseArtifactMutationFailureVault<'a> {
        inner: &'a MemoryVault,
    }

    impl CredentialVault for LeaseArtifactMutationFailureVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            if key == LEASE_DIRECTORY_VAULT_KEY {
                return Err("lease directory store failure".to_string());
            }
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            if matches!(key, LEASE_VAULT_KEY | LEASE_DIRECTORY_VAULT_KEY) {
                return Err("lease artifact delete failure".to_string());
            }
            self.inner.delete(key)
        }
    }

    struct LeaseArtifactAndDeviceDeleteFailureVault<'a> {
        inner: &'a MemoryVault,
    }

    impl CredentialVault for LeaseArtifactAndDeviceDeleteFailureVault<'_> {
        type Error = String;

        fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
            if key == LEASE_DIRECTORY_VAULT_KEY {
                return Err("lease directory store failure".to_string());
            }
            self.inner.store(key, secret)
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.load(key)
        }

        fn delete(&self, key: &str) -> Result<(), Self::Error> {
            if matches!(
                key,
                LEASE_VAULT_KEY | LEASE_DIRECTORY_VAULT_KEY | DEVICE_VAULT_KEY
            ) {
                return Err("selected delete failure".to_string());
            }
            self.inner.delete(key)
        }
    }

    struct AllMutationFailureVault<'a> {
        inner: &'a MemoryVault,
    }

    impl CredentialVault for AllMutationFailureVault<'_> {
        type Error = String;

        fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
            Err("vault store failure".to_string())
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.load(key)
        }

        fn delete(&self, _key: &str) -> Result<(), Self::Error> {
            Err("vault delete failure".to_string())
        }
    }

    fn service_with_marker_state(rejected_restore_marker: RejectedRestoreMarker) -> AccountService {
        use super::oidc::OidcEndpoints;

        let service = AccountService::new_with_rejected_restore_marker(
            AccountServiceConfig::try_new(
                "https://auth.axiusflow.com/api/auth",
                "axiusflow-desktop",
            )
            .expect("test config builds"),
            rejected_restore_marker,
        );
        // Stub discovery so unit tests never touch the network.
        service
            .endpoints
            .lock()
            .expect("endpoint cache locks")
            .replace(OidcEndpoints {
                issuer: "https://auth.axiusflow.com/api/auth".to_string(),
                authorization_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/authorize"
                    .to_string(),
                token_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/token".to_string(),
                jwks_uri: "https://auth.axiusflow.com/api/auth/jwks".to_string(),
                revocation_endpoint: "https://auth.axiusflow.com/api/auth/oauth2/revoke"
                    .to_string(),
                link_endpoint: "https://auth.axiusflow.com/api/axiusflow/link".to_string(),
                lease_endpoint: "https://auth.axiusflow.com/api/axiusflow/lease".to_string(),
            });
        service
    }

    fn service_with_marker(rejected_restore_marker: Option<PathBuf>) -> AccountService {
        service_with_marker_state(
            rejected_restore_marker
                .map_or(RejectedRestoreMarker::Disabled, RejectedRestoreMarker::Path),
        )
    }

    fn service() -> AccountService {
        service_with_marker(None)
    }

    fn marker_fixture() -> (PathBuf, PathBuf) {
        let sequence = MARKER_FIXTURE_SEQUENCE.fetch_add(1, Ordering::AcqRel);
        let root = std::env::temp_dir().join(format!(
            "axiusflow-account-restore-marker-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("marker fixture directory creates");
        let marker = root.join("rejected-restore");
        (root, marker)
    }

    fn restoring_service() -> AccountService {
        restoring_service_with_marker(None)
    }

    fn restoring_service_with_marker(rejected_restore_marker: Option<PathBuf>) -> AccountService {
        let service = service_with_marker(rejected_restore_marker);
        service.set_restore_readiness(AccountRestoreReadiness::Pending);
        service.state.lock().expect("account state locks").view = super::cleared_view(
            AccountSessionState::Authorizing,
            0,
            "restoring saved sign-in",
        );
        service
    }

    fn signed_lease_fixture(
        seed: [u8; 32],
        key_id: &str,
        account_id: &str,
        device_key: &[u8],
        plan: &str,
        revision: u64,
        expires_at: u64,
    ) -> (String, super::LeaseKey) {
        let signing_key = SigningKey::from_bytes(&seed);
        let device_id = super::lease::device_id_for_key(device_key);
        let now = super::unix_now();
        let header = serde_json::json!({ "alg": "EdDSA", "kid": key_id, "typ": "JWT" });
        let payload = serde_json::json!({
            "ver": 1,
            "aid": account_id,
            "did": device_id,
            "plan": plan,
            "feat": 7,
            "rev": revision,
            "iat": now.saturating_sub(1),
            "nbf": now.saturating_sub(1),
            "exp": expires_at,
            "aud": "axiusflow-engine",
            "kid": key_id,
        });
        let encode = |value: &serde_json::Value| {
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("lease fixture encodes"))
        };
        let input = format!("{}.{}", encode(&header), encode(&payload));
        let signature = signing_key.sign(input.as_bytes());
        let compact = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        (
            compact,
            (key_id.to_string(), signing_key.verifying_key().to_bytes()),
        )
    }

    fn seed_current_cached_lease_without_profile(vault: &MemoryVault, expires_at: u64) {
        let device_key = b"cached-lease-device-key";
        let (compact, key) = signed_lease_fixture(
            [9_u8; 32],
            "ent1",
            "acct_cached",
            device_key,
            "pro",
            1,
            expires_at,
        );
        let directory = vec![key];

        vault
            .store(DEVICE_VAULT_KEY, device_key)
            .expect("device fixture stores");
        vault
            .store(LEASE_VAULT_KEY, compact.as_bytes())
            .expect("lease fixture stores");
        vault
            .store(
                LEASE_DIRECTORY_VAULT_KEY,
                &serde_json::to_vec(&directory).expect("directory fixture encodes"),
            )
            .expect("directory fixture stores");
    }

    fn seed_cached_profile(vault: &MemoryVault, account_id: &str, name: &str, email: &str) {
        super::store_profile_material(
            vault,
            &AccountId::try_new(account_id).expect("profile account fixture builds"),
            &profile(name, email),
        )
        .expect("profile fixture stores");
    }

    fn seed_current_cached_lease(vault: &MemoryVault, expires_at: u64) {
        seed_current_cached_lease_without_profile(vault, expires_at);
        seed_cached_profile(vault, "acct_cached", "Ada Trader", "ada@example.test");
    }

    #[test]
    fn empty_local_restore_settles_signed_out_and_ready_without_network() {
        let service = restoring_service();
        let vault = MemoryVault::default();

        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::SignedOut
        );
    }

    #[test]
    fn unavailable_rejected_restore_marker_path_blocks_cached_offline_restore() {
        let service = service_with_marker_state(RejectedRestoreMarker::Unavailable);
        service.set_restore_readiness(AccountRestoreReadiness::Pending);
        service.state.lock().expect("account state locks").view = super::cleared_view(
            AccountSessionState::Authorizing,
            0,
            "restoring saved sign-in",
        );
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");

        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(!service.is_authenticated());
    }

    #[test]
    fn valid_cached_local_restore_preserves_offline_access_and_is_ready() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(2));

        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::OfflineLease
        );
        assert_eq!(service.account_status().display_name, "Ada Trader");
        assert!(service.is_authenticated());
    }

    #[test]
    fn cached_lease_without_profile_cannot_authenticate_startup() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease_without_profile(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");

        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline {
                cached_expiry: None
            }
        ));
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::Authorizing
        );
        assert!(!service.is_authenticated());
    }

    #[test]
    fn unavailable_local_vault_fails_restore_readiness() {
        let service = restoring_service();

        assert!(matches!(
            service.restore_local_session(&UnavailableVault),
            LocalRestore::Settled
        ));
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
    }

    #[test]
    fn cached_lease_read_failures_fail_readiness_but_allow_valid_refresh_to_continue() {
        for failing_key in [LEASE_VAULT_KEY, LEASE_DIRECTORY_VAULT_KEY, DEVICE_VAULT_KEY] {
            let service = restoring_service();
            let vault = MemoryVault::default();
            seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
            vault
                .store(REFRESH_VAULT_KEY, b"cached-refresh")
                .expect("refresh fixture stores");
            let failing = SelectiveLoadFailureVault {
                inner: &vault,
                failing_key,
            };

            let LocalRestore::ContinueOnline { cached_expiry } =
                service.restore_local_session(&failing)
            else {
                panic!("a readable refresh token must preserve online recovery");
            };
            assert_eq!(
                service
                    .current_restore_refresh_token(&failing)
                    .expect("refresh read succeeds")
                    .expect("refresh token remains available")
                    .as_str(),
                "cached-refresh"
            );
            assert_eq!(cached_expiry, None);
            assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
            assert_eq!(
                service.account_status().state,
                AccountSessionState::Authorizing
            );
        }
    }

    #[test]
    fn refresh_vault_failure_is_not_hidden_by_a_valid_cached_lease() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(2));
        let failing = SelectiveLoadFailureVault {
            inner: &vault,
            failing_key: REFRESH_VAULT_KEY,
        };

        assert!(matches!(
            service.restore_local_session(&failing),
            LocalRestore::Settled
        ));
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::OfflineLease
        );
        assert!(service.is_authenticated());
    }

    #[test]
    fn active_refresh_rejection_cannot_fall_back_to_cached_offline_access() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"rejected-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");

        let outcome = super::accept_refresh_grant(
            &service,
            0,
            &session,
            &vault,
            Err(oidc::RefreshGrantError::Rejected),
            super::unix_now(),
        );

        assert!(matches!(
            outcome,
            Err(super::lease::RefreshOutcome::Unavailable)
        ));
        assert_eq!(
            service.account_status().state,
            AccountSessionState::ReauthenticationRequired
        );
        assert!(!service.is_authenticated());
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .is_none()
        );
        assert!(vault.load(LEASE_VAULT_KEY).expect("lease reads").is_none());
    }

    #[test]
    fn transient_refresh_unavailability_still_uses_valid_cached_lease() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        seed_cached_profile(&vault, "acct_cached", "Ada Trader", "ada@example.test");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");

        let outcome = super::accept_refresh_grant(
            &service,
            0,
            &session,
            &vault,
            Err(oidc::RefreshGrantError::Unavailable),
            super::unix_now(),
        );

        assert!(matches!(
            outcome,
            Err(super::lease::RefreshOutcome::OfflineCovered(PlanId::Pro))
        ));
        assert!(service.is_authenticated());
    }

    #[test]
    fn directory_unavailability_during_key_rotation_keeps_valid_cached_coverage() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        let now = super::unix_now();
        seed_current_cached_lease(&vault, now.saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        seed_cached_profile(&vault, "acct_cached", "Ada Trader", "ada@example.test");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let old_lease = vault
            .load(LEASE_VAULT_KEY)
            .expect("cached lease reads")
            .expect("cached lease exists");
        let (rotated_compact, _rotated_key) = signed_lease_fixture(
            [41_u8; 32],
            "ent2",
            "acct_cached",
            b"cached-lease-device-key",
            "elite",
            2,
            now.saturating_add(60),
        );

        let outcome = super::validate_and_cache_after_directory_fetch(
            &service,
            0,
            &session,
            &rotated_compact,
            &vault,
            now,
            Err(()),
        );

        assert_eq!(
            outcome,
            super::lease::RefreshOutcome::OfflineCovered(PlanId::Pro),
            "a transient directory outage must not invalidate a still-valid cached lease"
        );
        assert!(service.is_authenticated());
        assert_eq!(
            vault.load(LEASE_VAULT_KEY).expect("lease reads").as_deref(),
            Some(old_lease.as_slice()),
            "an unverifiable rotated lease must not replace cached offline authority"
        );
    }

    #[test]
    fn lease_session_binds_refresh_token_read_to_its_generation() {
        let service = service();
        let vault = MemoryVault::default();
        vault
            .store(REFRESH_VAULT_KEY, b"generation-refresh")
            .expect("refresh fixture stores");
        vault
            .store(DEVICE_VAULT_KEY, b"generation-device-key")
            .expect("device fixture stores");
        {
            let mut state = service.state.lock().expect("account state locks");
            state.last_generation = 61;
            state.view = AccountView {
                state: AccountSessionState::Active,
                account_id: "acct_01".to_string(),
                plan_id: "pro".to_string(),
                detail: "signed in".to_string(),
                request_generation: 61,
                display_name: String::new(),
                email: String::new(),
                photo_url: String::new(),
            };
        }
        let racing = GenerationRaceVault {
            inner: &vault,
            state: Arc::clone(&service.state),
            attempted_advance: AtomicBool::new(false),
            advanced: AtomicBool::new(false),
            replacement_generation: 62,
        };

        let session = super::lease_session(&service, 61, &racing).expect("lease session builds");

        assert!(racing.attempted_advance.load(Ordering::Acquire));
        assert!(
            !racing.advanced.load(Ordering::Acquire),
            "a newer generation must not replace durable refresh material while stale credential selection owns the lifecycle fence"
        );
        assert_eq!(session.refresh_token, "generation-refresh");
        assert_eq!(
            service
                .state
                .lock()
                .expect("account state locks")
                .last_generation,
            61
        );
    }

    #[test]
    fn initial_lease_warmup_device_storage_failure_retires_active_access() {
        let service = service();
        let vault = MemoryVault::default();
        vault
            .store(REFRESH_VAULT_KEY, b"warmup-refresh")
            .expect("refresh fixture stores");
        vault
            .store(DEVICE_VAULT_KEY, b"warmup-device-key")
            .expect("device fixture stores");
        let account = AccountId::try_new("acct_01").expect("account fixture builds");
        {
            let mut state = service.state.lock().expect("account state locks");
            state.last_generation = 71;
            state.view = AccountView {
                state: AccountSessionState::Active,
                account_id: account.as_str().to_string(),
                plan_id: "pro".to_string(),
                detail: "signed in".to_string(),
                request_generation: 71,
                display_name: String::new(),
                email: String::new(),
                photo_url: String::new(),
            };
        }
        let failing = SelectiveLoadFailureVault {
            inner: &vault,
            failing_key: DEVICE_VAULT_KEY,
        };
        let tokens = verified_restore("acct_01", None).3;

        service.refresh_lease_once(71, &tokens, &account, &super::oidc_agent(), &failing);

        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError,
            "post-login warmup must not ignore an observed protected-storage failure"
        );
        assert!(!service.is_authenticated());
    }

    #[test]
    fn active_refresh_credential_reads_fail_closed_instead_of_looking_transient() {
        for failing_key in [REFRESH_VAULT_KEY, DEVICE_VAULT_KEY] {
            let service = restoring_service();
            let vault = MemoryVault::default();
            seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
            vault
                .store(REFRESH_VAULT_KEY, b"cached-refresh")
                .expect("refresh fixture stores");
            assert!(matches!(
                service.restore_local_session(&vault),
                LocalRestore::ContinueOnline { .. }
            ));
            let failing = SelectiveLoadFailureVault {
                inner: &vault,
                failing_key,
            };

            let outcome = super::refresh_lease_round(&service, 0, &failing);

            assert_eq!(outcome, super::lease::RefreshOutcome::Unavailable);
            assert_eq!(
                service.account_status().state,
                AccountSessionState::TerminalError,
                "protected credential reads must retire current access instead of becoming a cosmetic network outage"
            );
            assert!(!service.is_authenticated());
        }
    }

    #[test]
    fn cached_lease_read_failure_during_transient_refresh_fails_current_access_closed() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let failing = SelectiveLoadFailureVault {
            inner: &vault,
            failing_key: LEASE_VAULT_KEY,
        };

        let outcome = super::accept_refresh_grant(
            &service,
            0,
            &session,
            &failing,
            Err(oidc::RefreshGrantError::Unavailable),
            super::unix_now(),
        );

        assert_eq!(outcome, Err(super::lease::RefreshOutcome::Unavailable));
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(!service.is_authenticated());
    }

    #[test]
    fn stale_refresh_rejection_cannot_retire_newer_session_credentials() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"old-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let stale_session =
            super::lease_session(&service, 0, &vault).expect("stale lease session builds");
        {
            let mut state = service.state.lock().expect("account state locks");
            state.last_generation = 11;
            state.view.request_generation = 11;
            state.view.state = AccountSessionState::Active;
        }
        vault
            .store(REFRESH_VAULT_KEY, b"new-refresh")
            .expect("new refresh fixture stores");

        let _ = super::accept_refresh_grant(
            &service,
            0,
            &stale_session,
            &vault,
            Err(oidc::RefreshGrantError::Rejected),
            super::unix_now(),
        );

        assert!(service.is_authenticated());
        assert_eq!(service.account_status().request_generation, 11);
        assert_eq!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .as_deref(),
            Some(b"new-refresh".as_slice())
        );
    }

    #[test]
    fn rotated_refresh_store_failure_fails_current_session_closed() {
        let (root, marker) = marker_fixture();
        let service = restoring_service_with_marker(Some(marker.clone()));
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"old-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let failing = AllMutationFailureVault { inner: &vault };
        let tokens = VerifiedTokens {
            subject: "subject".to_string(),
            access: "access".to_string(),
            id_token: "identity".to_string(),
            refresh: Some("rotated-refresh".to_string()),
        };

        let outcome = super::accept_refresh_grant(
            &service,
            0,
            &session,
            &failing,
            Ok(tokens),
            super::unix_now(),
        );

        assert!(matches!(
            outcome,
            Err(super::lease::RefreshOutcome::Unavailable)
        ));
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(!service.is_authenticated());
        assert_eq!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .as_deref(),
            Some(b"old-refresh".as_slice())
        );

        let restarted = restoring_service_with_marker(Some(marker));
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::TerminalError,
            "a failed rotated-token commit must remain fail-closed across restart"
        );
        assert!(!restarted.is_authenticated());
        fs::remove_dir_all(root).expect("marker fixture removes");
    }

    #[test]
    fn rotated_directory_and_lease_remain_restart_compatible_at_every_commit_boundary() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        let now = super::unix_now();
        seed_current_cached_lease(&vault, now.saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let old_compact = String::from_utf8(
            vault
                .load(LEASE_VAULT_KEY)
                .expect("old lease reads")
                .expect("old lease exists"),
        )
        .expect("old lease is utf8");
        let (new_compact, new_key) = signed_lease_fixture(
            [19_u8; 32],
            "ent2",
            "acct_cached",
            b"cached-lease-device-key",
            "pro",
            2,
            now.saturating_add(60),
        );

        let outcome = super::validate_and_cache_with_directory(
            &service,
            0,
            &session,
            &new_compact,
            &vault,
            now,
            std::slice::from_ref(&new_key),
        );
        assert!(matches!(
            outcome,
            super::lease::RefreshOutcome::Refreshed(PlanId::Pro)
        ));
        let transition_directory: Vec<super::LeaseKey> = serde_json::from_slice(
            &vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .expect("directory exists"),
        )
        .expect("transition directory decodes");
        assert_eq!(transition_directory.len(), 2);
        assert!(
            super::lease::validate_compact(
                &old_compact,
                &transition_directory,
                &session.account_id,
                &session.device_id,
                now,
            )
            .is_ok(),
            "directory-first commit must retain authority for the old durable lease"
        );
        assert!(
            super::lease::validate_compact(
                &new_compact,
                &transition_directory,
                &session.account_id,
                &session.device_id,
                now,
            )
            .is_ok(),
            "the same durable directory must authorize the replacement lease"
        );

        let outcome = super::validate_and_cache_with_directory(
            &service,
            0,
            &session,
            &new_compact,
            &vault,
            now,
            std::slice::from_ref(&new_key),
        );
        assert!(matches!(
            outcome,
            super::lease::RefreshOutcome::Current(Some(PlanId::Pro))
        ));
        let converged: Vec<super::LeaseKey> = serde_json::from_slice(
            &vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .expect("directory exists"),
        )
        .expect("directory decodes");
        assert_eq!(converged, vec![new_key]);
    }

    #[test]
    fn signing_key_rotation_cannot_roll_back_the_cached_lease_revision() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        let now = super::unix_now();
        let device_key = b"cached-lease-device-key";
        let (old_compact, old_key) = signed_lease_fixture(
            [9_u8; 32],
            "ent1",
            "acct_cached",
            device_key,
            "elite",
            9,
            now.saturating_add(60),
        );
        vault
            .store(DEVICE_VAULT_KEY, device_key)
            .expect("device fixture stores");
        vault
            .store(LEASE_VAULT_KEY, old_compact.as_bytes())
            .expect("old lease fixture stores");
        vault
            .store(
                LEASE_DIRECTORY_VAULT_KEY,
                &serde_json::to_vec(&vec![old_key.clone()]).expect("old directory encodes"),
            )
            .expect("old directory fixture stores");
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        seed_cached_profile(&vault, "acct_cached", "Ada Trader", "ada@example.test");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let (rollback_compact, new_key) = signed_lease_fixture(
            [19_u8; 32],
            "ent2",
            "acct_cached",
            device_key,
            "starter",
            3,
            now.saturating_add(60),
        );

        let outcome = super::validate_and_cache_with_directory(
            &service,
            0,
            &session,
            &rollback_compact,
            &vault,
            now,
            std::slice::from_ref(&new_key),
        );

        assert_eq!(
            outcome,
            super::lease::RefreshOutcome::Current(Some(PlanId::Elite)),
            "key rotation must not let a lower signed revision replace a newer cached entitlement"
        );
        assert_eq!(
            vault.load(LEASE_VAULT_KEY).expect("lease reads").as_deref(),
            Some(old_compact.as_bytes()),
            "the durable lease must remain at the highest verified revision"
        );
        let transition_directory: Vec<super::LeaseKey> = serde_json::from_slice(
            &vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .expect("directory exists"),
        )
        .expect("transition directory decodes");
        assert_eq!(transition_directory.len(), 2);
        assert!(
            super::lease::validate_compact(
                &old_compact,
                &transition_directory,
                &session.account_id,
                &session.device_id,
                now,
            )
            .is_ok(),
            "the transition directory must keep the highest-revision durable lease restorable"
        );
    }

    #[test]
    fn same_key_id_with_different_signing_key_is_rejected_before_durable_rotation() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        let now = super::unix_now();
        seed_current_cached_lease(&vault, now.saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let old_lease = vault
            .load(LEASE_VAULT_KEY)
            .expect("old lease reads")
            .expect("old lease exists");
        let old_directory = vault
            .load(LEASE_DIRECTORY_VAULT_KEY)
            .expect("old directory reads")
            .expect("old directory exists");
        let (replacement, replacement_key) = signed_lease_fixture(
            [29_u8; 32],
            "ent1",
            "acct_cached",
            b"cached-lease-device-key",
            "pro",
            2,
            now.saturating_add(60),
        );

        let outcome = super::validate_and_cache_with_directory(
            &service,
            0,
            &session,
            &replacement,
            &vault,
            now,
            std::slice::from_ref(&replacement_key),
        );

        assert_eq!(outcome, super::lease::RefreshOutcome::Unavailable);
        assert_eq!(
            vault.load(LEASE_VAULT_KEY).expect("lease reads").as_deref(),
            Some(old_lease.as_slice())
        );
        assert_eq!(
            vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .as_deref(),
            Some(old_directory.as_slice())
        );
        assert!(service.is_authenticated());
    }

    #[test]
    fn directory_store_failure_cannot_publish_or_persist_replacement_lease() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        let now = super::unix_now();
        seed_current_cached_lease(&vault, now.saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let old_lease = vault
            .load(LEASE_VAULT_KEY)
            .expect("old lease reads")
            .expect("old lease exists");
        let old_directory = vault
            .load(LEASE_DIRECTORY_VAULT_KEY)
            .expect("old directory reads")
            .expect("old directory exists");
        let (new_compact, new_key) = signed_lease_fixture(
            [23_u8; 32],
            "ent2",
            "acct_cached",
            b"cached-lease-device-key",
            "pro",
            2,
            now.saturating_add(60),
        );
        let failing = LeaseArtifactMutationFailureVault { inner: &vault };

        let outcome = super::validate_and_cache_with_directory(
            &service,
            0,
            &session,
            &new_compact,
            &failing,
            now,
            std::slice::from_ref(&new_key),
        );

        assert_eq!(outcome, super::lease::RefreshOutcome::Unavailable);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(!service.is_authenticated());
        assert_eq!(
            vault.load(LEASE_VAULT_KEY).expect("lease reads").as_deref(),
            Some(old_lease.as_slice())
        );
        assert_eq!(
            vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .as_deref(),
            Some(old_directory.as_slice())
        );
    }

    #[test]
    fn replacement_lease_store_failure_retires_cached_entitlement_across_restart() {
        let (root, marker) = marker_fixture();
        let service = restoring_service_with_marker(Some(marker.clone()));
        let vault = MemoryVault::default();
        let now = super::unix_now();
        seed_current_cached_lease(&vault, now.saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let session = super::lease_session(&service, 0, &vault).expect("lease session builds");
        let (replacement, replacement_key) = signed_lease_fixture(
            [31_u8; 32],
            "ent2",
            "acct_cached",
            b"cached-lease-device-key",
            "starter",
            2,
            now.saturating_add(60),
        );
        let failing = SelectiveStoreFailureVault {
            inner: &vault,
            failing_key: LEASE_VAULT_KEY,
        };

        let outcome = super::validate_and_cache_with_directory(
            &service,
            0,
            &session,
            &replacement,
            &failing,
            now,
            std::slice::from_ref(&replacement_key),
        );

        assert_eq!(outcome, super::lease::RefreshOutcome::Unavailable);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError,
            "a verified replacement lease that cannot become durable must retire current access"
        );
        assert!(!service.is_authenticated());
        assert!(vault.load(LEASE_VAULT_KEY).expect("lease reads").is_none());
        // Once the cached lease itself is deleted, retaining the bounded key
        // directory is harmless: there is no durable entitlement left for it
        // to authorize on restart.
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .is_none()
        );

        let restarted = restoring_service_with_marker(Some(marker));
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert!(!restarted.is_authenticated());
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::SignedOut
        );
        fs::remove_dir_all(root).expect("marker fixture removes");
    }

    #[test]
    fn malformed_or_empty_refresh_material_requires_reauthentication_but_is_locally_ready() {
        for refresh in [Vec::new(), vec![0xff]] {
            let service = restoring_service();
            let vault = MemoryVault::default();
            vault
                .store(REFRESH_VAULT_KEY, &refresh)
                .expect("refresh fixture stores");

            assert!(matches!(
                service.restore_local_session(&vault),
                LocalRestore::Settled
            ));
            assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
            assert_eq!(
                service.account_status().state,
                AccountSessionState::ReauthenticationRequired
            );
        }
    }

    #[test]
    fn valid_refresh_material_marks_local_readiness_before_online_restore() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        vault
            .store(REFRESH_VAULT_KEY, b"cached-refresh")
            .expect("refresh fixture stores");

        let LocalRestore::ContinueOnline { cached_expiry } = service.restore_local_session(&vault)
        else {
            panic!("valid refresh material continues to the online phase");
        };
        assert_eq!(
            service
                .current_restore_refresh_token(&vault)
                .expect("refresh read succeeds")
                .expect("refresh token remains available")
                .as_str(),
            "cached-refresh"
        );
        assert_eq!(cached_expiry, None);
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::Authorizing
        );
    }

    #[test]
    fn verified_online_and_cached_offline_states_authenticate() {
        let service = service();
        {
            let mut state = service.state.lock().expect("account state locks");
            state.view.state = AccountSessionState::OfflineLease;
            state.lease_expires_at = Some(super::unix_now().saturating_add(60));
        }
        assert!(service.is_authenticated());

        service
            .state
            .lock()
            .expect("account state locks")
            .view
            .state = AccountSessionState::Active;
        assert!(service.is_authenticated());
    }

    #[test]
    fn offline_lease_without_a_deadline_fails_closed() {
        let service = service();
        {
            let mut state = service.state.lock().expect("account state locks");
            state.view = AccountView {
                state: AccountSessionState::OfflineLease,
                account_id: "acct_01".to_string(),
                plan_id: "pro".to_string(),
                detail: "cached access".to_string(),
                request_generation: 0,
                display_name: String::new(),
                email: String::new(),
                photo_url: String::new(),
            };
            state.lease_expires_at = None;
        }

        assert!(!service.is_authenticated());
        assert_eq!(
            service.account_status().state,
            AccountSessionState::ReauthenticationRequired
        );
    }

    #[test]
    fn expired_lease_state_fails_closed_without_an_expiry_worker() {
        for session_state in [
            AccountSessionState::OfflineLease,
            AccountSessionState::Active,
        ] {
            let service = service();
            {
                let mut state = service.state.lock().expect("account state locks");
                state.view = AccountView {
                    state: session_state,
                    account_id: "acct_01".to_string(),
                    plan_id: "pro".to_string(),
                    detail: "verified access".to_string(),
                    request_generation: 7,
                    display_name: String::new(),
                    email: String::new(),
                    photo_url: String::new(),
                };
                state.last_generation = 7;
                state.lease_expires_at = Some(super::unix_now());
            }

            assert!(
                !service.is_authenticated(),
                "expired {session_state:?} must not depend on a background worker to retire access"
            );
            let view = service.account_status();
            assert_eq!(view.state, AccountSessionState::ReauthenticationRequired);
            assert!(view.account_id.is_empty());
            assert!(view.plan_id.is_empty());
        }
    }

    #[test]
    fn lease_worker_start_failure_retires_current_access() {
        let service = service();
        {
            let mut state = service.state.lock().expect("account state locks");
            state.last_generation = 9;
            state.view = AccountView {
                state: AccountSessionState::Active,
                account_id: "acct_01".to_string(),
                plan_id: "elite".to_string(),
                detail: "signed in".to_string(),
                request_generation: 9,
                display_name: "Ada".to_string(),
                email: "ada@example.test".to_string(),
                photo_url: String::new(),
            };
            state.lease_expires_at = Some(super::unix_now().saturating_add(60));
        }

        service.fail_lease_worker_start(9);
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::TerminalError);
        assert!(view.account_id.is_empty());
        assert!(view.plan_id.is_empty());
        assert!(!service.is_authenticated());
    }

    #[test]
    fn transient_online_restore_failure_does_not_discard_cached_access() {
        let service = service();
        {
            let mut state = service.state.lock().expect("account state locks");
            state.view = AccountView {
                state: AccountSessionState::OfflineLease,
                account_id: "acct_01".to_string(),
                plan_id: "pro".to_string(),
                detail: "cached access".to_string(),
                request_generation: 0,
                display_name: String::new(),
                email: String::new(),
                photo_url: String::new(),
            };
            state.lease_expires_at = Some(super::unix_now().saturating_add(60));
        }

        service.complete_restore_without_session(
            AccountSessionState::TerminalError,
            "online verification is temporarily unavailable",
        );
        assert!(service.is_authenticated());
        assert_eq!(
            service.account_status().state,
            AccountSessionState::OfflineLease
        );
    }

    #[test]
    fn lease_refresh_wait_uses_zero_jitter_when_rng_is_unavailable() {
        assert_eq!(
            super::lease_refresh_wait_seconds(None),
            super::LEASE_REFRESH_INTERVAL_SECONDS
        );
        assert_eq!(
            super::lease_refresh_wait_seconds(Some([0_u8; 8])),
            super::LEASE_REFRESH_INTERVAL_SECONDS
        );
    }

    #[test]
    fn profile_refresh_claims_only_one_current_signed_in_request() {
        let in_flight = AtomicBool::new(false);
        let signed_out = AccountView {
            state: AccountSessionState::SignedOut,
            account_id: String::new(),
            plan_id: String::new(),
            detail: String::new(),
            request_generation: 4,
            display_name: String::new(),
            email: String::new(),
            photo_url: String::new(),
        };
        assert_eq!(claim_profile_refresh(&signed_out, &in_flight), None);
        assert!(!in_flight.load(Ordering::Acquire));

        let active = AccountView {
            state: AccountSessionState::Active,
            account_id: "acct_01".to_string(),
            request_generation: 7,
            ..signed_out
        };
        assert_eq!(claim_profile_refresh(&active, &in_flight), Some(7));
        assert_eq!(claim_profile_refresh(&active, &in_flight), None);
        in_flight.store(false, Ordering::Release);
        assert_eq!(claim_profile_refresh(&active, &in_flight), Some(7));
    }

    #[test]
    fn linked_profile_refresh_is_account_and_generation_fenced() {
        let service = service();
        let vault = MemoryVault::default();
        let account = AccountId::try_new("acct_01").expect("account builds");
        {
            let mut state = service.state.lock().expect("state locks");
            state.last_generation = 9;
            state.restore_allowed = false;
            state.view = AccountView {
                state: AccountSessionState::Active,
                account_id: account.as_str().to_string(),
                plan_id: "starter".to_string(),
                detail: "signed in".to_string(),
                request_generation: 9,
                display_name: "Old Name".to_string(),
                email: "old@example.test".to_string(),
                photo_url: "https://example.test/old.png".to_string(),
            };
        }

        assert!(service.apply_linked_profile(
            &vault,
            9,
            &account,
            &account,
            profile("new", "new@example.test"),
        ));
        let refreshed = service.account_status();
        assert_eq!(refreshed.plan_id, "starter");
        assert_eq!(refreshed.display_name, "new");

        let other = AccountId::try_new("acct_02").expect("other account builds");
        assert!(!service.apply_linked_profile(
            &vault,
            9,
            &account,
            &other,
            profile("wrong", "wrong@example.test"),
        ));
        assert!(!service.apply_linked_profile(
            &vault,
            8,
            &account,
            &account,
            profile("stale", "stale@example.test"),
        ));
        let still_current = service.account_status();
        assert_eq!(still_current.plan_id, "starter");
        assert_eq!(still_current.display_name, "new");

        service.state.lock().expect("state locks").view.state = AccountSessionState::SignedOut;
        assert!(!service.apply_linked_profile(
            &vault,
            9,
            &account,
            &account,
            profile("late", "late@example.test"),
        ));
    }

    #[test]
    fn profile_refresh_applies_only_verified_online_lease_plan_changes() {
        use super::lease::RefreshOutcome;

        let service = service();
        {
            let mut state = service.state.lock().expect("state locks");
            state.last_generation = 9;
            state.view = AccountView {
                state: AccountSessionState::Active,
                account_id: "acct_01".to_string(),
                plan_id: "starter".to_string(),
                detail: "signed in".to_string(),
                request_generation: 9,
                display_name: "Ada".to_string(),
                email: "ada@example.test".to_string(),
                photo_url: String::new(),
            };
        }

        service.apply_profile_refresh_outcome(9, RefreshOutcome::Refreshed(PlanId::Pro));
        let refreshed = service.account_status();
        assert_eq!(refreshed.state, AccountSessionState::Active);
        assert_eq!(refreshed.plan_id, "pro");

        service.apply_profile_refresh_outcome(9, RefreshOutcome::OfflineCovered(PlanId::Elite));
        let transient = service.account_status();
        assert_eq!(transient.state, AccountSessionState::Active);
        assert_eq!(transient.plan_id, "pro");

        service.apply_profile_refresh_outcome(9, RefreshOutcome::Unavailable);
        let unavailable = service.account_status();
        assert_eq!(unavailable.state, AccountSessionState::Active);
        assert_eq!(unavailable.plan_id, "pro");

        service.apply_lease_outcome(9, RefreshOutcome::Deferred);
        let deferred = service.account_status();
        assert_eq!(deferred.state, AccountSessionState::Active);
        assert_eq!(deferred.plan_id, "pro");

        service.apply_profile_refresh_outcome(9, RefreshOutcome::Current(Some(PlanId::Elite)));
        let current = service.account_status();
        assert_eq!(current.state, AccountSessionState::Active);
        assert_eq!(current.plan_id, "elite");
    }

    #[test]
    fn refresh_quiesce_waits_for_inflight_grant_and_drop_reenables_refresh() {
        let service = service();
        let permit = service
            .claim_refresh_grant()
            .expect("refresh lifecycle state locks")
            .expect("unquiesced runtime accepts one grant");
        let quiesce = service
            .begin_refresh_quiesce()
            .expect("shutdown quiesce claim installs");

        assert!(
            service
                .claim_refresh_grant()
                .expect("refresh lifecycle state locks")
                .is_none(),
            "no new refresh grant may start after lifecycle quiescing begins"
        );
        assert!(
            quiesce.wait_for(Duration::ZERO).is_err(),
            "shutdown cannot complete while a grant is still inside its rotation-commit phase"
        );

        drop(permit);
        assert_eq!(quiesce.wait_for(Duration::from_millis(50)), Ok(()));
        drop(quiesce);

        assert!(
            service
                .claim_refresh_grant()
                .expect("refresh lifecycle state locks")
                .is_some(),
            "cancelling a prepared restart must release its quiesce claim"
        );
    }

    #[test]
    fn refresh_quiesce_claims_are_reference_counted() {
        let service = service();
        let shutdown = service
            .begin_refresh_quiesce()
            .expect("shutdown quiesce claim installs");
        let update = service
            .begin_refresh_quiesce()
            .expect("update quiesce claim installs");

        drop(update);
        assert!(
            service
                .claim_refresh_grant()
                .expect("refresh lifecycle state locks")
                .is_none(),
            "releasing an update claim must not release a simultaneous shutdown claim"
        );

        drop(shutdown);
        assert!(
            service
                .claim_refresh_grant()
                .expect("refresh lifecycle state locks")
                .is_some(),
            "refresh grants resume only after the final quiesce owner releases"
        );
    }

    #[test]
    fn refresh_quiesce_and_grant_admission_are_serialized() {
        for _ in 0..32 {
            let service = service();
            let barrier = Arc::new(Barrier::new(3));
            let grant_service = service.clone();
            let grant_barrier = Arc::clone(&barrier);
            let grant = std::thread::spawn(move || {
                grant_barrier.wait();
                grant_service
                    .claim_refresh_grant()
                    .expect("refresh lifecycle state locks")
            });
            let quiesce_service = service.clone();
            let quiesce_barrier = Arc::clone(&barrier);
            let quiesce = std::thread::spawn(move || {
                quiesce_barrier.wait();
                quiesce_service
                    .begin_refresh_quiesce()
                    .expect("quiesce claim installs")
            });

            barrier.wait();
            let permit = grant.join().expect("grant admission thread completes");
            let quiesce = quiesce.join().expect("quiesce thread completes");
            if let Some(permit) = permit {
                assert!(
                    quiesce.wait_for(Duration::ZERO).is_err(),
                    "a grant admitted before quiesce must remain visible to its waiter"
                );
                drop(permit);
                assert_eq!(quiesce.wait_for(Duration::from_millis(50)), Ok(()));
            } else {
                assert_eq!(
                    quiesce.wait_for(Duration::ZERO),
                    Ok(()),
                    "a quiesce admitted first must prevent the competing grant"
                );
            }
        }
    }

    #[test]
    fn online_restore_reloads_refresh_material_after_quiesce_release() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        vault
            .store(REFRESH_VAULT_KEY, b"pre-quiesce-refresh")
            .expect("initial refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let quiesce = service
            .begin_refresh_quiesce()
            .expect("update quiesce claim installs");
        drop(quiesce);
        vault
            .store(REFRESH_VAULT_KEY, b"post-quiesce-refresh")
            .expect("rotated refresh fixture stores");

        assert_eq!(
            service
                .current_restore_refresh_token(&vault)
                .expect("refresh read succeeds")
                .expect("refresh token remains available")
                .as_str(),
            "post-quiesce-refresh",
            "startup restore must not retain a refresh token captured before lifecycle quiescing"
        );
    }

    #[test]
    fn authenticated_check_fails_closed_when_runtime_state_is_busy() {
        let service = service();
        {
            let mut state = service.state.lock().expect("state locks");
            state.last_generation = 9;
            state.view = AccountView {
                state: AccountSessionState::Active,
                account_id: "acct_01".to_string(),
                plan_id: "pro".to_string(),
                detail: "signed in".to_string(),
                request_generation: 9,
                display_name: String::new(),
                email: String::new(),
                photo_url: String::new(),
            };
            state.lease_expires_at = Some(super::unix_now().saturating_add(60));
        }

        let _busy = service.state.lock().expect("state locks");
        assert_eq!(
            service.try_is_authenticated(),
            None,
            "the nonblocking presentation probe distinguishes lifecycle contention from signed-out state"
        );
        assert!(
            !service.is_authenticated(),
            "the GPUI hard-auth check must fail closed instead of waiting on the runtime lifecycle lock"
        );
    }

    fn verified_restore(
        account_id: &str,
        refresh: Option<&str>,
    ) -> (AccountId, PlanId, AccountProfile, VerifiedTokens) {
        (
            AccountId::try_new(account_id).expect("account fixture builds"),
            PlanId::Starter,
            AccountProfile {
                display_name: "Ada Trader".to_string(),
                email: "ada@example.test".to_string(),
                photo_url: "https://example.test/ada.png".to_string(),
            },
            VerifiedTokens {
                subject: "subject-1".to_string(),
                access: "access-token".to_string(),
                id_token: "verified-id-token".to_string(),
                refresh: refresh.map(str::to_string),
            },
        )
    }

    #[test]
    fn verified_online_restore_rotates_vault_material_before_activating() {
        let service = service();
        let vault = MemoryVault::default();
        let (account, plan, profile, tokens) =
            verified_restore("acct_restore", Some("rotated-refresh"));
        let restored = service.apply_online_restore(&vault, Ok(tokens), |_| {
            assert_eq!(
                vault.load(REFRESH_VAULT_KEY).expect("vault"),
                Some(b"rotated-refresh".to_vec())
            );
            assert!(!service.is_authenticated());
            Ok((account, plan, profile))
        });

        assert!(restored.is_some());
        assert!(service.is_authenticated());
        let view = service.account_status();
        assert_eq!(view.account_id, "acct_restore");
        assert_eq!(view.plan_id, "starter");
        assert_eq!(view.display_name, "Ada Trader");
        assert_eq!(
            vault.load(REFRESH_VAULT_KEY).expect("vault reads"),
            Some(b"rotated-refresh".to_vec())
        );
    }

    #[test]
    fn failed_or_retired_online_restore_never_authenticates() {
        let failed = service();
        let vault = MemoryVault::default();
        assert!(
            failed
                .apply_online_restore(&vault, Err(oidc::RefreshGrantError::Rejected), |_| panic!(
                    "no link after rejection"
                ))
                .is_none()
        );
        assert!(!failed.is_authenticated());
        assert_eq!(
            failed.account_status().state,
            AccountSessionState::ReauthenticationRequired
        );

        let unavailable = service();
        assert!(
            unavailable
                .apply_online_restore(
                    &UnavailableVault,
                    Ok(verified_restore("acct_unstored", Some("rotated-refresh")).3),
                    |_| panic!("no link without durable token"),
                )
                .is_none()
        );
        assert!(!unavailable.is_authenticated());
        assert_eq!(
            unavailable.account_status().state,
            AccountSessionState::TerminalError
        );

        let retired = service();
        {
            let mut state = retired.state.lock().expect("account state locks");
            state.last_generation = 7;
            state.restore_allowed = false;
            state.view = super::cleared_view(AccountSessionState::SignedOut, 7, "signed out");
        }
        assert!(
            retired
                .apply_online_restore(
                    &vault,
                    Ok(verified_restore("acct_retired", Some("stale-refresh")).3),
                    |_| panic!("no link for retired restore"),
                )
                .is_none()
        );
        assert!(!retired.is_authenticated());
        assert_eq!(retired.account_status().request_generation, 7);
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("vault reads")
                .is_none(),
            "a retired restore cannot rotate current vault material"
        );
    }

    #[test]
    fn rejected_online_restore_removes_every_restorable_session_credential() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"rejected-refresh")
            .expect("refresh fixture stores");

        let LocalRestore::ContinueOnline { .. } = service.restore_local_session(&vault) else {
            panic!("cached lease plus refresh must continue online restore");
        };
        assert_eq!(
            service
                .current_restore_refresh_token(&vault)
                .expect("refresh read succeeds")
                .expect("refresh token remains available")
                .as_str(),
            "rejected-refresh"
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::OfflineLease
        );

        assert!(
            service
                .apply_online_restore(&vault, Err(oidc::RefreshGrantError::Rejected), |_| panic!(
                    "an invalid grant must never reach account linking"
                ))
                .is_none()
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::ReauthenticationRequired
        );
        assert!(!service.is_authenticated());
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .is_none()
        );
        assert!(vault.load(LEASE_VAULT_KEY).expect("lease reads").is_none());

        let restarted = restoring_service();
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::SignedOut,
            "authoritatively rejected credentials must not authenticate after restart"
        );
        assert!(!restarted.is_authenticated());
    }

    #[test]
    fn rejected_online_restore_cleanup_failure_clears_access_and_reports_storage_failure() {
        for failing_key in [REFRESH_VAULT_KEY, LEASE_VAULT_KEY] {
            let service = restoring_service();
            let vault = MemoryVault::default();
            seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
            vault
                .store(REFRESH_VAULT_KEY, b"rejected-refresh")
                .expect("refresh fixture stores");
            assert!(matches!(
                service.restore_local_session(&vault),
                LocalRestore::ContinueOnline { .. }
            ));
            assert!(service.is_authenticated());
            let failing = SelectiveDeleteFailureVault {
                inner: &vault,
                failing_key,
            };

            assert!(
                service
                    .apply_online_restore(
                        &failing,
                        Err(oidc::RefreshGrantError::Rejected),
                        |_| panic!("an invalid grant must never reach account linking")
                    )
                    .is_none()
            );
            let view = service.account_status();
            assert_eq!(view.state, AccountSessionState::TerminalError);
            assert_eq!(
                view.detail,
                "saved sign-in could not be retired; credential storage is unavailable; retry sign-in"
            );
            assert!(!service.is_authenticated());
            assert!(
                vault
                    .load(failing_key)
                    .expect("failed key remains")
                    .is_some(),
                "cleanup failure must not be represented as durable retirement"
            );
            let other_key = if failing_key == REFRESH_VAULT_KEY {
                LEASE_VAULT_KEY
            } else {
                REFRESH_VAULT_KEY
            };
            assert!(
                vault.load(other_key).expect("other key reads").is_none(),
                "both cleanup attempts must run even when one deletion fails"
            );

            let restarted = restoring_service();
            let restart = restarted.restore_local_session(&vault);
            assert!(
                !restarted.is_authenticated(),
                "known rejected credentials must never recover offline access after restart"
            );
            if failing_key == LEASE_VAULT_KEY {
                assert!(matches!(restart, LocalRestore::Settled));
                assert_eq!(
                    restarted.account_status().state,
                    AccountSessionState::SignedOut
                );
                assert!(
                    vault
                        .load(LEASE_DIRECTORY_VAULT_KEY)
                        .expect("lease directory reads")
                        .is_none(),
                    "a failed lease deletion must retire the cached lease validation artifact"
                );
                assert!(
                    service
                        .lease_keys
                        .lock()
                        .expect("lease keys lock")
                        .is_empty(),
                    "the rejected lease validation directory must also retire in memory"
                );

                service.begin_login(61).expect("fresh sign-in starts");
                assert!(service.complete_with_tokens(
                    61,
                    &AccountId::try_new("acct_cached").expect("cached account id builds"),
                    PlanId::Elite,
                    &profile("fresh", "fresh@example.test"),
                    Some("fresh-refresh"),
                    &vault,
                ));
                let session = super::lease_session(&service, 61, &vault)
                    .expect("fresh active session builds");
                assert!(matches!(
                    super::cached_outcome(&service, 61, &session, &vault, super::unix_now()),
                    super::lease::RefreshOutcome::Unavailable
                ));
                assert_eq!(
                    service.account_status().state,
                    AccountSessionState::Active,
                    "a transient post-login lease miss must not reuse the retired cached lease"
                );
            } else {
                assert!(matches!(restart, LocalRestore::ContinueOnline { .. }));
            }
        }
    }

    #[test]
    fn rejected_restore_replaces_directory_when_lease_artifact_deletes_fail() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"rejected-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let failing = LeaseArtifactDeleteFailureVault { inner: &vault };

        assert!(
            service
                .apply_online_restore(
                    &failing,
                    Err(oidc::RefreshGrantError::Rejected),
                    |_| panic!("an invalid grant must never reach account linking")
                )
                .is_none()
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert_eq!(
            vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads"),
            Some(b"[]".to_vec()),
            "replacement must invalidate a lease when both lease-artifact deletes fail"
        );
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .is_none()
        );

        let restarted = restoring_service();
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::SignedOut
        );
        assert!(!restarted.is_authenticated());
    }

    #[test]
    fn rejected_restore_retires_device_when_lease_directory_cannot_mutate() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"rejected-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let failing = LeaseArtifactMutationFailureVault { inner: &vault };

        assert!(
            service
                .apply_online_restore(
                    &failing,
                    Err(oidc::RefreshGrantError::Rejected),
                    |_| panic!("an invalid grant must never reach account linking")
                )
                .is_none()
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(
            vault.load(LEASE_VAULT_KEY).expect("lease reads").is_some(),
            "the failing backend preserves the rejected lease entry"
        );
        assert!(
            vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .is_some(),
            "the failing backend preserves the rejected lease directory"
        );
        assert!(
            vault
                .load(DEVICE_VAULT_KEY)
                .expect("device key reads")
                .is_none(),
            "device binding must retire when lease and directory cannot mutate"
        );
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .is_none()
        );

        let restarted = restoring_service();
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::SignedOut
        );
        assert!(!restarted.is_authenticated());
    }

    #[test]
    fn rejected_restore_marker_blocks_restart_when_no_lease_validation_artifact_can_mutate() {
        let (marker_root, marker_path) = marker_fixture();
        let service = restoring_service_with_marker(Some(marker_path.clone()));
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"rejected-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let failing = AllMutationFailureVault { inner: &vault };

        assert!(
            service
                .apply_online_restore(
                    &failing,
                    Err(oidc::RefreshGrantError::Rejected),
                    |_| panic!("an invalid grant must never reach account linking")
                )
                .is_none()
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert_eq!(
            vault.load(REFRESH_VAULT_KEY).expect("refresh reads"),
            Some(b"rejected-refresh".to_vec()),
            "failed vault cleanup must preserve rejected refresh evidence"
        );
        assert!(
            marker_path.is_file(),
            "independent rejection marker must persist"
        );
        assert!(vault.load(LEASE_VAULT_KEY).expect("lease reads").is_some());
        assert!(
            vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .is_some()
        );
        assert!(
            vault
                .load(DEVICE_VAULT_KEY)
                .expect("device key reads")
                .is_some()
        );

        let restarted = restoring_service_with_marker(Some(marker_path.clone()));
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.restore_readiness(),
            AccountRestoreReadiness::Failed
        );
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(!restarted.is_authenticated());

        let signed_in = service_with_marker(Some(marker_path.clone()));
        signed_in.begin_login(71).expect("fresh sign-in starts");
        assert!(signed_in.complete_with_tokens(
            71,
            &AccountId::try_new("acct_cached").expect("cached account id builds"),
            PlanId::Elite,
            &profile("fresh", "fresh@example.test"),
            Some("fresh-refresh"),
            &vault,
        ));
        assert_eq!(
            signed_in.account_status().state,
            AccountSessionState::Active
        );
        assert!(
            !marker_path.exists(),
            "verified sign-in must clear the rejected-restore marker before Active publishes"
        );

        let _ = fs::remove_dir_all(marker_root);
    }

    #[test]
    fn rejected_restore_rotates_device_when_device_delete_also_fails() {
        let service = restoring_service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        let old_device = vault
            .load(DEVICE_VAULT_KEY)
            .expect("device key reads")
            .expect("device key exists");
        vault
            .store(REFRESH_VAULT_KEY, b"rejected-refresh")
            .expect("refresh fixture stores");
        assert!(matches!(
            service.restore_local_session(&vault),
            LocalRestore::ContinueOnline { .. }
        ));
        let failing = LeaseArtifactAndDeviceDeleteFailureVault { inner: &vault };

        assert!(
            service
                .apply_online_restore(
                    &failing,
                    Err(oidc::RefreshGrantError::Rejected),
                    |_| panic!("an invalid grant must never reach account linking")
                )
                .is_none()
        );
        let replacement_device = vault
            .load(DEVICE_VAULT_KEY)
            .expect("device key reads")
            .expect("replacement device key exists");
        assert_ne!(replacement_device, old_device);
        assert!(vault.load(LEASE_VAULT_KEY).expect("lease reads").is_some());
        assert!(
            vault
                .load(LEASE_DIRECTORY_VAULT_KEY)
                .expect("directory reads")
                .is_some()
        );
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .is_none()
        );

        let restarted = restoring_service();
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::SignedOut
        );
        assert!(!restarted.is_authenticated());
    }

    #[test]
    fn stale_rejected_restore_cannot_delete_newer_session_credentials() {
        let service = service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"newer-refresh")
            .expect("refresh fixture stores");
        {
            let mut state = service.state.lock().expect("account state locks");
            state.last_generation = 7;
            state.restore_allowed = false;
            state.view = super::cleared_view(AccountSessionState::SignedOut, 7, "signed out");
        }

        assert!(
            service
                .apply_online_restore(&vault, Err(oidc::RefreshGrantError::Rejected), |_| panic!(
                    "retired restore must never reach account linking"
                ))
                .is_none()
        );
        assert_eq!(service.account_status().request_generation, 7);
        assert_eq!(
            vault.load(REFRESH_VAULT_KEY).expect("refresh reads"),
            Some(b"newer-refresh".to_vec())
        );
        assert!(vault.load(LEASE_VAULT_KEY).expect("lease reads").is_some());
    }

    #[test]
    fn online_restore_preserves_rotation_when_account_link_fails() {
        let service = service();
        let vault = MemoryVault::default();
        vault
            .store(REFRESH_VAULT_KEY, b"old-refresh")
            .expect("vault");
        assert!(
            service
                .apply_online_restore(
                    &vault,
                    Ok(verified_restore("acct_restore", Some("rotated-refresh")).3),
                    |_| Err("unavailable".to_string())
                )
                .is_none()
        );
        assert_eq!(
            vault.load(REFRESH_VAULT_KEY).expect("vault"),
            Some(b"rotated-refresh".to_vec())
        );
        assert!(!service.is_authenticated());
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
    }

    #[test]
    fn online_restore_rotation_store_failure_cannot_restore_cached_access_after_restart() {
        let (root, marker) = marker_fixture();
        let service = restoring_service_with_marker(Some(marker.clone()));
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        vault
            .store(REFRESH_VAULT_KEY, b"old-refresh")
            .expect("refresh fixture stores");
        let failing = AllMutationFailureVault { inner: &vault };
        assert!(matches!(
            service.restore_local_session(&failing),
            LocalRestore::ContinueOnline { .. }
        ));

        assert!(
            service
                .apply_online_restore(
                    &failing,
                    Ok(verified_restore("acct_cached", Some("rotated-refresh")).3),
                    |_| panic!("storage failure must stop restore before account linking")
                )
                .is_none()
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        assert!(!service.is_authenticated());
        assert_eq!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh reads")
                .as_deref(),
            Some(b"old-refresh".as_slice())
        );

        let restarted = restoring_service_with_marker(Some(marker));
        assert!(matches!(
            restarted.restore_local_session(&vault),
            LocalRestore::Settled
        ));
        assert_eq!(
            restarted.account_status().state,
            AccountSessionState::TerminalError,
            "failed restore-time rotation must remain fail-closed across restart"
        );
        assert!(!restarted.is_authenticated());
        fs::remove_dir_all(root).expect("marker fixture removes");
    }

    #[test]
    fn restore_lookup_retries_are_bounded_and_cancel_when_retired() {
        let service = service();
        let mut calls = 0;
        assert_eq!(
            service.retry_restore_lookup(|| {
                calls += 1;
                if calls < 3 {
                    Err("temporary".to_string())
                } else {
                    Ok(7)
                }
            }),
            Ok(7)
        );
        assert_eq!(calls, 3);
        calls = 0;
        assert!(
            service
                .retry_restore_lookup::<()>(|| {
                    calls += 1;
                    Err("unavailable".to_string())
                })
                .is_err()
        );
        assert_eq!(calls, 3);
        calls = 0;
        assert!(
            service
                .retry_restore_lookup::<()>(|| {
                    calls += 1;
                    service.state.lock().expect("state").restore_allowed = false;
                    Err("retired during lookup".to_string())
                })
                .is_err()
        );
        assert_eq!(calls, 1);
        assert!(!service.is_authenticated());
    }

    #[test]
    fn online_restore_unavailability_is_not_expiry_and_late_link_cannot_authenticate() {
        let unavailable = service();
        let vault = MemoryVault::default();
        assert!(
            unavailable
                .apply_online_restore(
                    &vault,
                    Err(oidc::RefreshGrantError::Unavailable),
                    |_| panic!("no link without verification")
                )
                .is_none()
        );
        assert_eq!(
            unavailable.account_status().state,
            AccountSessionState::TerminalError
        );
        let retired = service();
        let (account, plan, profile, tokens) = verified_restore("acct_restore", None);
        assert!(
            retired
                .apply_online_restore(&vault, Ok(tokens), |_| {
                    let mut state = retired.state.lock().expect("state");
                    state.last_generation = 7;
                    state.restore_allowed = false;
                    state.view =
                        super::cleared_view(AccountSessionState::SignedOut, 7, "signed out");
                    Ok((account, plan, profile))
                })
                .is_none()
        );
        assert!(!retired.is_authenticated());
        assert_eq!(retired.account_status().request_generation, 7);
    }

    #[test]
    fn unconfigured_control_plane_fails_closed() {
        let service = AccountService::new(AccountServiceConfig {
            issuer: String::new(),
            client_id: "axiusflow-desktop".to_string(),
        });
        assert!(service.begin_login(1).is_err());
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
    }

    #[test]
    fn unconfigured_restore_fails_local_readiness_without_network() {
        let service = AccountService::new_restoring(AccountServiceConfig {
            issuer: String::new(),
            client_id: "axiusflow-desktop".to_string(),
        });
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
    }

    #[test]
    fn restore_can_be_started_once_after_process_lifecycle_fencing() {
        let service = AccountService::new(AccountServiceConfig {
            issuer: String::new(),
            client_id: "axiusflow-desktop".to_string(),
        });
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::SignedOut
        );

        service.start_restore();
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError
        );
        let first = service.account_status();

        service.start_restore();
        assert_eq!(service.account_status(), first);
        assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
    }

    #[test]
    fn duplicate_begin_and_retired_generations_fail_closed() {
        let service = service();
        service.begin_login(1).expect("first login starts");
        assert!(service.begin_login(2).is_err());
        assert!(service.begin_login(1).is_err());
        service
            .cancel_login(1)
            .expect("cancel clears the pending login");
        assert!(service.begin_login(1).is_err());
        assert!(service.cancel_login(1).is_err());
    }

    #[test]
    fn cancel_and_callback_mismatch_leave_signed_out_state() {
        let service = service();
        service.begin_login(4).expect("login starts");
        service.cancel_login(4).expect("cancel succeeds");
        assert_eq!(
            service.account_status().state,
            AccountSessionState::SignedOut
        );
        assert!(
            service
                .apply_callback_query("code=abc&state=xyz", 4)
                .is_err()
        );
    }

    #[test]
    fn cancelling_login_releases_the_loopback_listener_promptly() {
        let service = service();
        service.begin_login(5).expect("login starts");
        let redirect_uri = service
            .state
            .lock()
            .expect("state locks")
            .pending
            .as_ref()
            .expect("pending login exists")
            .redirect_uri
            .clone();
        let port = redirect_uri
            .strip_prefix("http://127.0.0.1:")
            .and_then(|value| value.strip_suffix("/callback"))
            .and_then(|value| value.parse::<u16>().ok())
            .expect("loopback redirect carries its bound port");

        service.cancel_login(5).expect("cancel succeeds");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => {
                    drop(listener);
                    break;
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => {
                    panic!("cancelled login retained its loopback listener past the bound: {error}")
                }
            }
        }
        assert_eq!(
            service.account_status().state,
            AccountSessionState::SignedOut
        );
    }

    #[test]
    fn callback_validates_state_and_rejects_duplicates() {
        let service = service();
        service.begin_login(7).expect("login starts");
        let oauth_state = {
            let state = service.state.lock().expect("state locks");
            state
                .pending
                .as_ref()
                .expect("pending exists")
                .oauth_state
                .clone()
        };
        let query = format!("code=code-1&state={oauth_state}");
        service
            .apply_callback_query(&query, 7)
            .expect("first callback applies");
        assert!(service.apply_callback_query(&query, 7).is_err());
        assert!(
            service
                .apply_callback_query("code=other&state=wrong", 7)
                .is_err()
        );
        assert!(service.apply_callback_query(&query, 8).is_err());
    }

    fn profile(name: &str, email: &str) -> super::oidc::AccountProfile {
        super::oidc::AccountProfile {
            display_name: name.to_string(),
            email: email.to_string(),
            photo_url: format!("https://auth.axiusflow.com/photo/{name}.png"),
        }
    }

    #[test]
    fn retired_completion_results_cannot_mutate_current_state() {
        let service = service();
        service.begin_login(9).expect("login starts");
        service.fail_generation(8, "stale worker result");
        assert_eq!(
            service.account_status().state,
            AccountSessionState::Authorizing
        );
        service.cancel_login(9).expect("cancel succeeds");
        let vault = MemoryVault::default();
        service.complete_with_tokens(
            9,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("stale", "stale@example.com"),
            Some("refresh"),
            &vault,
        );
        // A retired callback restores nothing: neither state nor another
        // user's profile may leak into the current session.
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::SignedOut);
        assert!(view.display_name.is_empty());
        assert!(view.email.is_empty());
        assert!(view.photo_url.is_empty());
    }

    #[test]
    fn active_completion_stores_refresh_and_device_material() {
        let service = service();
        service.begin_login(11).expect("login starts");
        let vault = MemoryVault::default();
        service.complete_with_tokens(
            11,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-value"),
            &vault,
        );
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::Active);
        assert_eq!(view.account_id, "acct_01");
        assert_eq!(view.plan_id, "pro");
        assert_eq!(view.display_name, "ada");
        assert_eq!(view.email, "ada@example.com");
        assert_eq!(view.photo_url, "https://auth.axiusflow.com/photo/ada.png");
    }

    #[test]
    fn vault_failure_reports_without_activating_or_leaking_profile() {
        let service = service();
        service.begin_login(12).expect("login starts");
        // Exchange and link succeeded; only credential storage failed.
        service.complete_with_tokens(
            12,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-value"),
            &super::UnavailableVault,
        );
        // Failure is actionable and carries no session: a partial login
        // never renders as success.
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::TerminalError);
        assert!(view.account_id.is_empty());
        assert!(view.display_name.is_empty());
        assert!(view.email.is_empty());
        assert!(view.photo_url.is_empty());
    }

    #[test]
    fn account_switch_replaces_profile_without_carryover() {
        let service = service();
        let vault = MemoryVault::default();
        service.begin_login(31).expect("first login starts");
        service.complete_with_tokens(
            31,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-ada"),
            &vault,
        );
        assert_eq!(service.account_status().display_name, "ada");
        service
            .endpoints
            .lock()
            .expect("endpoint cache locks")
            .take();
        let signed_out = service
            .sign_out_with(&vault)
            .expect("first account signs out durably");
        assert!(signed_out.display_name.is_empty());
        assert!(signed_out.email.is_empty());
        assert!(signed_out.photo_url.is_empty());
        service.begin_login(32).expect("second login starts");
        service.complete_with_tokens(
            32,
            &AccountId::try_new("acct_02").expect("identity builds"),
            PlanId::Starter,
            &profile("bob", "bob@example.com"),
            Some("refresh-bob"),
            &vault,
        );
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::Active);
        assert_eq!(view.account_id, "acct_02");
        assert_eq!(view.display_name, "bob");
        assert_eq!(view.email, "bob@example.com");
        assert!(view.photo_url.contains("bob"));
    }

    #[test]
    fn login_timeout_is_configured() {
        assert!(LOGIN_TIMEOUT.as_secs() >= 60);
    }

    #[test]
    fn sign_out_clears_state_and_deletes_vault_material() {
        use super::{DEVICE_VAULT_KEY, LEASE_VAULT_KEY, PROFILE_VAULT_KEY, REFRESH_VAULT_KEY};
        use axiusflow_platform_runtime::CredentialVault as _;

        let service = service();
        let vault = MemoryVault::default();
        service.begin_login(21).expect("login starts");
        service.complete_with_tokens(
            21,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-value"),
            &vault,
        );
        assert_eq!(service.account_status().state, AccountSessionState::Active);
        assert!(vault.load(REFRESH_VAULT_KEY).expect("load reads").is_some());
        assert!(vault.load(PROFILE_VAULT_KEY).expect("load reads").is_some());
        assert!(vault.load(DEVICE_VAULT_KEY).expect("load reads").is_some());
        // Drop the cached endpoints so no revocation worker touches the
        // network during the test.
        service
            .endpoints
            .lock()
            .expect("endpoint cache locks")
            .take();
        // Seed a profile first: sign-out must clear it with the session.
        service.state.lock().expect("state locks").view.display_name = "ada".to_string();
        let view = service
            .sign_out_with(&vault)
            .expect("account signs out durably");
        assert_eq!(view.state, AccountSessionState::SignedOut);
        assert!(view.account_id.is_empty());
        assert!(view.display_name.is_empty());
        assert!(view.email.is_empty());
        assert!(view.photo_url.is_empty());
        // Refresh and lease material are gone; the device key stays for
        // complete uninstall to remove.
        assert!(vault.load(REFRESH_VAULT_KEY).expect("load reads").is_none());
        assert!(vault.load(LEASE_VAULT_KEY).expect("load reads").is_none());
        assert!(vault.load(PROFILE_VAULT_KEY).expect("load reads").is_none());
        assert!(vault.load(DEVICE_VAULT_KEY).expect("load reads").is_some());
        assert_eq!(
            service.account_status().state,
            AccountSessionState::SignedOut
        );
        // A retired completion after sign-out cannot resurrect the session.
        service.complete_with_tokens(
            21,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-value"),
            &vault,
        );
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::SignedOut);
        assert!(view.display_name.is_empty());
    }

    #[test]
    fn sign_out_refresh_delete_failure_is_actionable_and_survives_restart() {
        let service = service();
        let vault = MemoryVault::default();
        service.begin_login(41).expect("login starts");
        service.complete_with_tokens(
            41,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-survives"),
            &vault,
        );
        service
            .endpoints
            .lock()
            .expect("endpoint cache locks")
            .take();
        let failing = SelectiveDeleteFailureVault {
            inner: &vault,
            failing_key: REFRESH_VAULT_KEY,
        };

        let error = service
            .sign_out_with(&failing)
            .expect_err("surviving refresh material must fail durable sign-out");
        assert_eq!(
            error,
            "sign-out could not remove saved credentials; retry sign-out"
        );
        let signed_out = service.account_status();
        assert_eq!(signed_out.state, AccountSessionState::SignedOut);
        assert!(signed_out.account_id.is_empty());
        assert!(signed_out.display_name.is_empty());
        assert!(
            vault
                .load(REFRESH_VAULT_KEY)
                .expect("refresh read succeeds")
                .is_some()
        );

        let restarted = restoring_service();
        let LocalRestore::ContinueOnline { .. } = restarted.restore_local_session(&vault) else {
            panic!("surviving refresh material remains restorable after restart");
        };
        assert_eq!(
            restarted
                .current_restore_refresh_token(&vault)
                .expect("refresh read succeeds")
                .expect("refresh token remains available")
                .as_str(),
            "refresh-survives"
        );
    }

    #[test]
    fn sign_out_lease_delete_failure_is_actionable() {
        let service = service();
        let vault = MemoryVault::default();
        seed_current_cached_lease(&vault, super::unix_now().saturating_add(60));
        let failing = SelectiveDeleteFailureVault {
            inner: &vault,
            failing_key: LEASE_VAULT_KEY,
        };

        let error = service
            .sign_out_with(&failing)
            .expect_err("surviving lease material must fail durable sign-out");
        assert_eq!(
            error,
            "sign-out could not remove saved credentials; retry sign-out"
        );
        assert_eq!(
            service.account_status().state,
            AccountSessionState::SignedOut
        );
        assert!(
            vault
                .load(LEASE_VAULT_KEY)
                .expect("lease read succeeds")
                .is_some()
        );
    }

    #[test]
    fn retired_lease_warmup_cannot_touch_a_newer_session() {
        use super::lease::RefreshOutcome;

        let service = service();
        let vault = MemoryVault::default();
        service.begin_login(51).expect("login starts");
        service.complete_with_tokens(
            51,
            &AccountId::try_new("acct_01").expect("identity builds"),
            PlanId::Pro,
            &profile("ada", "ada@example.com"),
            Some("refresh-value"),
            &vault,
        );
        // Current-generation warmup updates the plan and stays Active.
        service.apply_lease_outcome(51, RefreshOutcome::Refreshed(PlanId::Elite));
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::Active);
        assert_eq!(view.plan_id, "elite");
        // A late warmup from a retired generation lands after sign-out:
        // it must not resurrect identity, plan, or access.
        service
            .endpoints
            .lock()
            .expect("endpoint cache locks")
            .take();
        service
            .sign_out_with(&vault)
            .expect("account signs out durably");
        service.apply_lease_outcome(51, RefreshOutcome::Refreshed(PlanId::Elite));
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::SignedOut);
        assert!(view.account_id.is_empty());
        assert!(view.plan_id.is_empty());
        assert!(view.display_name.is_empty());
        assert!(!service.is_authenticated());
    }
}
