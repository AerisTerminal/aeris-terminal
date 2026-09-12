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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_account::{AccountId, PlanId};
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
}

type LeaseKey = (String, [u8; 32]);

/// Engine-owned account session shared by all desktop windows.
#[derive(Clone)]
pub struct AccountService {
    config: AccountServiceConfig,
    state: Arc<Mutex<ServiceState>>,
    endpoints: Arc<Mutex<Option<OidcEndpoints>>>,
    lease_keys: Arc<Mutex<Vec<LeaseKey>>>,
    refresh_gate: Arc<Mutex<()>>,
    profile_refresh_in_flight: Arc<AtomicBool>,
    restore_readiness: Arc<AtomicU8>,
}

enum LocalRestore {
    Settled,
    ContinueOnline {
        refresh_token: Zeroizing<String>,
        cached_expiry: Option<u64>,
    },
}

impl AccountService {
    /// Creates an account session starting signed out.
    #[must_use]
    pub fn new(config: AccountServiceConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(ServiceState {
                view: cleared_view(AccountSessionState::SignedOut, 0, "signed out"),
                pending: None,
                last_generation: 0,
                restore_allowed: true,
            })),
            endpoints: Arc::new(Mutex::new(None)),
            lease_keys: Arc::new(Mutex::new(Vec::new())),
            refresh_gate: Arc::new(Mutex::new(())),
            profile_refresh_in_flight: Arc::new(AtomicBool::new(false)),
            restore_readiness: Arc::new(AtomicU8::new(AccountRestoreReadiness::Ready as u8)),
        }
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
        service.set_restore_readiness(AccountRestoreReadiness::Pending);
        if let Ok(mut state) = service.state.lock() {
            state.view = cleared_view(
                AccountSessionState::Authorizing,
                0,
                "restoring saved sign-in",
            );
        }
        if !service.config.is_configured() {
            service.set_restore_readiness(AccountRestoreReadiness::Failed);
            service.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "sign-in is unavailable; the account service is not configured",
            );
            return service;
        }
        let restoring = service.clone();
        if std::thread::Builder::new()
            .name("axiusflow-account-restore".to_string())
            .spawn(move || restoring.restore_online_session())
            .is_err()
        {
            service.set_restore_readiness(AccountRestoreReadiness::Failed);
            service.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "saved sign-in restore could not start; retry sign-in",
            );
        }
        service
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

    fn restore_online_session(&self) {
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            self.set_restore_readiness(AccountRestoreReadiness::Failed);
            self.complete_restore_without_session(
                AccountSessionState::TerminalError,
                "credential storage is unavailable; retry sign-in",
            );
            return;
        };
        let LocalRestore::ContinueOnline {
            refresh_token,
            cached_expiry,
        } = self.restore_local_session(&vault)
        else {
            return;
        };
        // Candidate readiness ends at the local-vault boundary above. Everything
        // below may perform bounded remote work and must not make activation
        // depend on current control-plane reachability.
        let agent = oidc_agent();
        let endpoints = self.retry_restore_lookup(|| self.oidc_endpoints());
        let outcome = endpoints
            .as_ref()
            .map_err(|_| oidc::RefreshGrantError::Unavailable)
            .and_then(|endpoints| {
                refresh_grant(endpoints, &agent, &self.config.client_id, &refresh_token)
            });
        let Some((account_id, tokens)) = self.apply_online_restore(&vault, outcome, |tokens| {
            let endpoints = endpoints.as_ref().map_err(Clone::clone)?;
            self.retry_restore_lookup(|| {
                link_subject(endpoints, &agent, &tokens.id_token, &tokens.subject)
            })
        }) else {
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
        let service = self.clone();
        std::thread::Builder::new()
            .name("axiusflow-account-lease".to_string())
            .spawn(move || service.run_lease_worker(0))
            .ok();
    }

    fn restore_local_session<V>(&self, vault: &V) -> LocalRestore
    where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
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
        let refresh_token = match vault.load(REFRESH_VAULT_KEY) {
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
        let Some(refresh_token) = refresh_token else {
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
        LocalRestore::ContinueOnline {
            refresh_token,
            cached_expiry,
        }
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
            display_name: String::new(),
            email: String::new(),
            photo_url: String::new(),
        };
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
            let generation = state.last_generation;
            state.view = cleared_view(
                AccountSessionState::ReauthenticationRequired,
                generation,
                "cached sign-in expired; reconnect and sign in again",
            );
        }
    }

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
        let tokens = match outcome {
            Ok(tokens) => tokens,
            Err(error) => {
                let (state, detail) = match error {
                    oidc::RefreshGrantError::Rejected => (
                        AccountSessionState::ReauthenticationRequired,
                        "saved sign-in expired; sign in again",
                    ),
                    oidc::RefreshGrantError::Unavailable => (
                        AccountSessionState::TerminalError,
                        "saved sign-in could not be verified; check your connection and retry sign-in",
                    ),
                };
                self.complete_restore_without_session(state, detail);
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
                state.restore_allowed = false;
                state.view = cleared_view(
                    AccountSessionState::TerminalError,
                    0,
                    "credential storage is unavailable; retry sign-in",
                );
                return None;
            }
        }
        drop(state);
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
        state.restore_allowed = false;
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
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            matches!(
                state.view.state,
                AccountSessionState::Active | AccountSessionState::OfflineLease
            )
        })
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
        std::thread::Builder::new()
            .name("axiusflow-account-login".to_string())
            .spawn(move || service.run_login_transaction(request_generation, &listener))
            .map_err(|_| "sign-in worker could not start; retry sign-in".to_string())?;
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
    #[must_use]
    pub fn sign_out(&self) -> AccountView {
        let vault = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE);
        match vault {
            Ok(vault) => self.sign_out_with(&vault),
            Err(_) => self.sign_out_with(&UnavailableVault),
        }
    }

    fn sign_out_with<V>(&self, vault: &V) -> AccountView
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
            state.view = cleared_view(AccountSessionState::SignedOut, generation, "signed out");
        }
        let lease_deleted = vault.delete(LEASE_VAULT_KEY).is_ok();
        let refresh_deleted = vault.delete(REFRESH_VAULT_KEY).is_ok();
        // Revocation reuses already-cached endpoints only: sign-out never
        // performs discovery on the coordinator path.
        let cached = self
            .endpoints
            .lock()
            .map(|cached| cached.clone())
            .unwrap_or_default();
        let revocation = cached.zip(
            token
                .filter(|_| refresh_deleted)
                .and_then(|token| String::from_utf8(token).ok()),
        );
        let deleted = lease_deleted && refresh_deleted;
        let view = {
            let Ok(mut state) = self.state.lock() else {
                return cleared_view(
                    AccountSessionState::TerminalError,
                    0,
                    "account state is unavailable",
                );
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
            std::thread::Builder::new()
                .name("axiusflow-account-revoke".to_string())
                .spawn(move || {
                    if oidc::revoke_refresh(&endpoints, &token).is_err() {
                        eprintln!("Axiusflow sign-out revocation degraded");
                    }
                })
                .ok();
        }
        view
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
        let callback = match listener.accept_one(remaining) {
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
    /// redacted detail for the browser page and the engine view.
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
        let stored = store_refresh_material(vault, refresh_token).is_ok();
        if !stored {
            let generation = state.last_generation;
            state.pending = None;
            state.view = cleared_view(
                AccountSessionState::TerminalError,
                generation,
                "credential storage is unavailable; retry sign-in",
            );
            return false;
        }
        let generation = state.last_generation;
        state.pending = None;
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
        std::thread::Builder::new()
            .name("axiusflow-account-lease".to_string())
            .spawn(move || service.run_lease_worker(generation))
            .ok();
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
        if ensure_device_key(vault).is_err() {
            return;
        }
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
        // worker. Its profile assignment is already generation/account fenced.
        // Ignore the lease outcome here so a cosmetic refresh cannot downgrade
        // a healthy session because of a transient network failure.
        let _ = refresh_lease_round(self, generation, &vault);
    }

    fn apply_linked_profile(
        &self,
        generation: u64,
        expected_account: &AccountId,
        linked_account: &AccountId,
        plan: PlanId,
        profile: AccountProfile,
    ) -> bool {
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
        state.view.plan_id = plan.as_str().to_string();
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
                state.view.state = AccountSessionState::ReauthenticationRequired;
                state.view.detail = "sign-in expired; sign in again".to_string();
                note_lease("unavailable");
            }
        }
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
    if getrandom::fill(&mut jitter).is_err() {
        return false;
    }
    let wait =
        Duration::from_secs(LEASE_REFRESH_INTERVAL_SECONDS + u64::from_le_bytes(jitter) % 1800);
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !service.is_current(generation) {
            return false;
        }
        std::thread::sleep(Duration::from_secs(60).min(deadline - Instant::now()));
    }
    service.is_current(generation)
}

struct LeaseSession {
    account_id: AccountId,
    device_id: String,
    refresh_token: String,
    endpoints: OidcEndpoints,
    origin: String,
}

fn lease_session<V>(service: &AccountService, generation: u64, vault: &V) -> Option<LeaseSession>
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let account_id = {
        let state = service.state.lock().ok()?;
        if state.last_generation != generation {
            return None;
        }
        AccountId::try_new(&state.view.account_id).ok()?
    };
    let refresh_token = vault
        .load(REFRESH_VAULT_KEY)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|token| !token.is_empty())?;
    let endpoints = service.oidc_endpoints().ok()?;
    let origin = control_plane_origin(&endpoints.issuer).ok()?;
    if ensure_device_key(vault).is_err() {
        return None;
    }
    let device_id = vault
        .load(DEVICE_VAULT_KEY)
        .ok()
        .flatten()
        .map(|key| device_id_for_key(&key))?;
    Some(LeaseSession {
        account_id,
        device_id,
        refresh_token,
        endpoints,
        origin,
    })
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
    let fetched_keys = lease::fetch_directory(agent, &session.origin).unwrap_or_default();
    let (claims, cached_claims) = {
        let cached_keys = service
            .lease_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let validation_keys = if fetched_keys.is_empty() {
            cached_keys.as_slice()
        } else {
            fetched_keys.as_slice()
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
        let cached_claims = lease::load_cached(vault, LEASE_VAULT_KEY).and_then(|cached| {
            lease::validate_compact(
                &cached,
                validation_keys,
                &session.account_id,
                &session.device_id,
                now_unix_seconds,
            )
            .ok()
        });
        (claims, cached_claims)
    };
    // Persist a rotated directory only after it validates a lease from the
    // same response path. A malformed lease cannot poison offline restart.
    if !fetched_keys.is_empty()
        && let Ok(mut cached) = service.lease_keys.lock()
    {
        cached.clone_from(&fetched_keys);
        if let Ok(encoded) = serde_json::to_vec(&fetched_keys) {
            let _ = vault.store(LEASE_DIRECTORY_VAULT_KEY, &encoded);
        }
    }
    match cached_claims {
        Some(cached) if cached.revision() >= claims.revision() => {
            lease::RefreshOutcome::Current(Some(cached.plan()))
        }
        _ => {
            let secret = Zeroizing::new(compact.as_bytes().to_vec());
            let current = service.state.lock().is_ok_and(|state| {
                state.last_generation == generation
                    && (state.view.state == AccountSessionState::Active
                        || state.view.state == AccountSessionState::OfflineLease)
                    && vault.store(LEASE_VAULT_KEY, secret.as_slice()).is_ok()
            });
            if current {
                lease::RefreshOutcome::Refreshed(claims.plan())
            } else {
                lease::RefreshOutcome::Unavailable
            }
        }
    }
}

fn cached_lease_covers<V>(
    service: &AccountService,
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
    let keys = service
        .lease_keys
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lease::load_cached(vault, LEASE_VAULT_KEY)
        .and_then(|cached| {
            lease::validate_compact(
                &cached,
                &keys,
                &session.account_id,
                &session.device_id,
                now_unix_seconds,
            )
            .ok()
        })
        .filter(|claims| {
            claims.expires_at().saturating_sub(now_unix_seconds) <= LEASE_OFFLINE_VALIDITY_SECONDS
        })
        .map(|claims| claims.plan())
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
    let Some(session) = lease_session(service, generation, vault) else {
        return lease::RefreshOutcome::Unavailable;
    };
    let now = unix_now();
    // One connection pool per background round: grant, link, lease, and
    // directory reuse it instead of paying a fresh handshake per stage.
    let agent = oidc_agent();
    let Ok(tokens) = refresh_grant(
        &session.endpoints,
        &agent,
        &service.config.client_id,
        &session.refresh_token,
    ) else {
        return cached_outcome(service, &session, vault, now);
    };
    if let Some(rotated) = tokens.refresh.as_deref().filter(|token| !token.is_empty()) {
        let secret = Zeroizing::new(rotated.as_bytes().to_vec());
        let _ = service.state.lock().map(|state| {
            if state.last_generation == generation
                && (state.view.state == AccountSessionState::Active
                    || state.view.state == AccountSessionState::OfflineLease)
            {
                let _ = vault.store(REFRESH_VAULT_KEY, secret.as_slice());
            }
        });
    }
    if let Ok((account_id, plan, profile)) = link_subject(
        &session.endpoints,
        &agent,
        &tokens.id_token,
        &tokens.subject,
    ) {
        let _ = service.apply_linked_profile(
            generation,
            &session.account_id,
            &account_id,
            plan,
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
        return cached_outcome(service, &session, vault, now);
    };
    validate_and_cache(service, generation, &session, &agent, &compact, vault, now)
}

fn cached_outcome<V>(
    service: &AccountService,
    session: &LeaseSession,
    vault: &V,
    now_unix_seconds: u64,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    if let Some(plan) = cached_lease_covers(service, session, vault, now_unix_seconds) {
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
    let Some(session) = lease_session(service, generation, vault) else {
        return lease::RefreshOutcome::Unavailable;
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
        UnavailableVault, claim_profile_refresh,
        oidc::{self, AccountProfile, VerifiedTokens},
    };
    use axiusflow_account::{AccountId, PlanId};
    use axiusflow_contracts::{AccountSessionState, AccountView};
    use axiusflow_platform_runtime::CredentialVault;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

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

    fn service() -> AccountService {
        use super::oidc::OidcEndpoints;

        let service = AccountService::new(
            AccountServiceConfig::try_new(
                "https://auth.axiusflow.com/api/auth",
                "axiusflow-desktop",
            )
            .expect("test config builds"),
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

    fn restoring_service() -> AccountService {
        let service = service();
        service.set_restore_readiness(AccountRestoreReadiness::Pending);
        service.state.lock().expect("account state locks").view = super::cleared_view(
            AccountSessionState::Authorizing,
            0,
            "restoring saved sign-in",
        );
        service
    }

    fn seed_current_cached_lease(vault: &MemoryVault, expires_at: u64) {
        let seed = [9_u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let key_id = "ent1";
        let device_key = b"cached-lease-device-key";
        let device_id = super::lease::device_id_for_key(device_key);
        let now = super::unix_now();
        let header = serde_json::json!({ "alg": "EdDSA", "kid": key_id, "typ": "JWT" });
        let payload = serde_json::json!({
            "ver": 1,
            "aid": "acct_cached",
            "did": device_id,
            "plan": "pro",
            "feat": 7,
            "rev": 1,
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
        let directory = vec![(key_id.to_string(), signing_key.verifying_key().to_bytes())];

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
        assert!(service.is_authenticated());
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

            let LocalRestore::ContinueOnline {
                refresh_token,
                cached_expiry,
            } = service.restore_local_session(&failing)
            else {
                panic!("a readable refresh token must preserve online recovery");
            };
            assert_eq!(refresh_token.as_str(), "cached-refresh");
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

        let LocalRestore::ContinueOnline {
            refresh_token,
            cached_expiry,
        } = service.restore_local_session(&vault)
        else {
            panic!("valid refresh material continues to the online phase");
        };
        assert_eq!(refresh_token.as_str(), "cached-refresh");
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
            9,
            &account,
            &account,
            PlanId::Pro,
            profile("new", "new@example.test"),
        ));
        let refreshed = service.account_status();
        assert_eq!(refreshed.plan_id, "pro");
        assert_eq!(refreshed.display_name, "new");

        let other = AccountId::try_new("acct_02").expect("other account builds");
        assert!(!service.apply_linked_profile(
            9,
            &account,
            &other,
            PlanId::Elite,
            profile("wrong", "wrong@example.test"),
        ));
        assert!(!service.apply_linked_profile(
            8,
            &account,
            &account,
            PlanId::Elite,
            profile("stale", "stale@example.test"),
        ));
        let still_current = service.account_status();
        assert_eq!(still_current.plan_id, "pro");
        assert_eq!(still_current.display_name, "new");

        service.state.lock().expect("state locks").view.state = AccountSessionState::SignedOut;
        assert!(!service.apply_linked_profile(
            9,
            &account,
            &account,
            PlanId::Elite,
            profile("late", "late@example.test"),
        ));
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
        let signed_out = service.sign_out_with(&vault);
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
        use super::{DEVICE_VAULT_KEY, LEASE_VAULT_KEY, REFRESH_VAULT_KEY};
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
        let view = service.sign_out_with(&vault);
        assert_eq!(view.state, AccountSessionState::SignedOut);
        assert!(view.account_id.is_empty());
        assert!(view.display_name.is_empty());
        assert!(view.email.is_empty());
        assert!(view.photo_url.is_empty());
        // Refresh and lease material are gone; the device key stays for
        // complete uninstall to remove.
        assert!(vault.load(REFRESH_VAULT_KEY).expect("load reads").is_none());
        assert!(vault.load(LEASE_VAULT_KEY).expect("load reads").is_none());
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
        service.sign_out_with(&vault);
        service.apply_lease_outcome(51, RefreshOutcome::Refreshed(PlanId::Elite));
        let view = service.account_status();
        assert_eq!(view.state, AccountSessionState::SignedOut);
        assert!(view.account_id.is_empty());
        assert!(view.plan_id.is_empty());
        assert!(view.display_name.is_empty());
        assert!(!service.is_authenticated());
    }
}
