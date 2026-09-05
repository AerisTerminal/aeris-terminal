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
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axiusflow_account::{AccountId, PlanId};
use axiusflow_engine_protocol::{AccountSessionState, AccountView, LoginAuthorization};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use zeroize::Zeroizing;

use lease::{LEASE_OFFLINE_VALIDITY_SECONDS, LEASE_REFRESH_INTERVAL_SECONDS, device_id_for_key};
use loopback::{LoopbackListener, validate_callback_query};
use oidc::{
    AccountProfile, AuthorizationRequest, OidcEndpoints, authorization_url, control_plane_origin,
    discover, exchange_code, link_subject, refresh_grant,
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
const DEVICE_VAULT_KEY: &str = "account-device-key-v1";

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
}

type LeaseKey = (String, [u8; 32]);

/// Engine-owned account session shared by all desktop windows.
#[derive(Clone)]
pub struct AccountService {
    config: AccountServiceConfig,
    state: Arc<Mutex<ServiceState>>,
    endpoints: Arc<Mutex<Option<OidcEndpoints>>>,
    lease_keys: Arc<Mutex<Vec<LeaseKey>>>,
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
            })),
            endpoints: Arc::new(Mutex::new(None)),
            lease_keys: Arc::new(Mutex::new(Vec::new())),
        }
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

    /// Signs out the shared session, deleting vault refresh and lease material.
    ///
    /// Local deletion happens synchronously so a concurrent login cannot
    /// observe stale material; server-side revocation follows on a bounded
    /// worker with the in-memory refresh copy and never blocks IPC.
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
        let lease_deleted = vault.delete(LEASE_VAULT_KEY).is_ok();
        let refresh_deleted = vault.delete(REFRESH_VAULT_KEY).is_ok();
        // Revocation reuses already-cached endpoints only: sign-out never
        // performs discovery on the IPC path.
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
            let generation = state.last_generation;
            state.pending = None;
            // Profile leaves with the session: a later sign-in as another
            // user never inherits these fields, even briefly.
            state.view = cleared_view(
                AccountSessionState::SignedOut,
                generation,
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

    fn run_login_transaction(&self, generation: u64, listener: &LoopbackListener) {
        let remaining = self.pending_remaining(generation);
        if remaining.is_zero() {
            self.fail_generation(generation, "sign-in timed out; retry sign-in");
            return;
        }
        let query = match listener.accept_one(remaining) {
            Ok(query) => query,
            Err(detail) => {
                self.fail_generation(generation, &detail);
                return;
            }
        };
        let code = match self.apply_callback_query(&query, generation) {
            Ok(code) => code,
            Err(detail) => {
                self.fail_generation(generation, &detail);
                return;
            }
        };
        self.exchange_and_link(generation, &code);
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

    fn exchange_and_link(&self, generation: u64, code: &str) {
        let (redirect_uri, verifier, nonce) = {
            let Ok(state) = self.state.lock() else {
                return;
            };
            let Some(pending) = state
                .pending
                .as_ref()
                .filter(|pending| pending.generation == generation)
            else {
                return;
            };
            (
                pending.redirect_uri.clone(),
                pending.verifier.encoded().to_string(),
                pending.nonce.clone(),
            )
        };
        let outcome = self.oidc_endpoints().and_then(|endpoints| {
            exchange_code(
                &endpoints,
                &self.config.client_id,
                &redirect_uri,
                code,
                &verifier,
                &nonce,
            )
            .and_then(|tokens| {
                link_subject(&endpoints, &tokens.id_token, &tokens.subject)
                    .map(|(account, plan, profile)| (account, plan, profile, tokens))
            })
        });
        match outcome {
            Ok((account_id, plan, profile, tokens)) => {
                let vault = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE);
                match vault {
                    Ok(vault) => {
                        self.complete_with_tokens(
                            generation,
                            &account_id,
                            plan,
                            &profile,
                            tokens.refresh.as_deref(),
                            &vault,
                        );
                        // Best-effort initial lease for the shadow cache; a
                        // failure never blocks the Active session.
                        self.refresh_lease_once(generation, &tokens, &account_id, &vault);
                    }
                    Err(_) => self.fail_generation(
                        generation,
                        "credential storage is unavailable; retry sign-in",
                    ),
                }
            }
            Err(detail) => self.fail_generation(generation, &detail),
        }
    }

    /// Publishes the Active session only after the refresh material is
    /// stored: exchange, link, and vault all precede any success the
    /// desktop can render. A retired generation assigns nothing, so a late
    /// callback can never restore another user's profile.
    fn complete_with_tokens<V>(
        &self,
        generation: u64,
        account_id: &AccountId,
        plan: PlanId,
        profile: &AccountProfile,
        refresh_token: Option<&str>,
        vault: &V,
    ) where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
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
        let stored = store_refresh_material(vault, refresh_token).is_ok();
        if !stored {
            let generation = state.last_generation;
            state.pending = None;
            state.view = cleared_view(
                AccountSessionState::TerminalError,
                generation,
                "credential storage is unavailable; retry sign-in",
            );
            return;
        }
        let generation = state.last_generation;
        state.pending = None;
        state.view = AccountView {
            state: AccountSessionState::Active as i32,
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
    }

    /// Returns whether one generation still owns the shared session.
    fn is_current(&self, generation: u64) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.last_generation == generation
                && (state.view.state == AccountSessionState::Active as i32
                    || state.view.state == AccountSessionState::OfflineLease as i32)
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
        let Ok(vault) = NativeCredentialVault::new(ACCOUNT_VAULT_SERVICE) else {
            note_lease_shadow("vault-unavailable");
            return;
        };
        let outcome = refresh_lease_round(self, generation, &vault);
        self.apply_lease_outcome(generation, outcome);
    }

    /// Best-effort initial lease right after sign-in (shadow cache warmup).
    fn refresh_lease_once<V>(
        &self,
        generation: u64,
        tokens: &oidc::VerifiedTokens,
        account_id: &AccountId,
        vault: &V,
    ) where
        V: CredentialVault,
        V::Error: std::fmt::Display,
    {
        if ensure_device_key(vault).is_err() {
            return;
        }
        let outcome = initial_lease_round(self, generation, tokens, account_id, vault);
        self.apply_lease_outcome(generation, outcome);
    }

    fn apply_lease_outcome(&self, generation: u64, outcome: lease::RefreshOutcome) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.last_generation != generation {
            return;
        }
        let current = state.view.state;
        let active = AccountSessionState::Active as i32;
        let offline = AccountSessionState::OfflineLease as i32;
        if current != active && current != offline {
            return;
        }
        match outcome {
            lease::RefreshOutcome::Refreshed | lease::RefreshOutcome::Current => {
                if current != active {
                    state.view.state = active;
                    state.view.detail = "signed in".to_string();
                }
                note_lease_shadow("valid");
            }
            lease::RefreshOutcome::OfflineCovered => {
                if current != offline {
                    state.view.state = offline;
                    state.view.detail =
                        "signed in with a cached subscription; reconnect to refresh".to_string();
                }
                note_lease_shadow("offline-covered");
            }
            lease::RefreshOutcome::Unavailable => {
                state.view.state = AccountSessionState::ReauthenticationRequired as i32;
                state.view.detail = "sign-in expired; sign in again".to_string();
                note_lease_shadow("unavailable");
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

/// One sanitized view with identity, plan, and profile cleared. Every
/// non-Active assignment flows through here so sign-out, cancel, expiry,
/// and failure can never leak a previous user's profile.
fn cleared_view(state: AccountSessionState, generation: u64, detail: &str) -> AccountView {
    AccountView {
        state: state as i32,
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

/// Redacted shadow observation: outcome class only, never identities.
fn note_lease_shadow(outcome: &str) {
    eprintln!("Axiusflow lease shadow: {outcome}");
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
    session: &LeaseSession,
    compact: &str,
    vault: &V,
    now_unix_seconds: u64,
) -> lease::RefreshOutcome
where
    V: CredentialVault,
    V::Error: std::fmt::Display,
{
    let keys = lease::fetch_directory(&session.origin).unwrap_or_default();
    if !keys.is_empty()
        && let Ok(mut cached) = service.lease_keys.lock()
    {
        *cached = keys;
    }
    let keys = service
        .lease_keys
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(claims) = lease::validate_compact(
        compact,
        &keys,
        &session.account_id,
        &session.device_id,
        now_unix_seconds,
    ) else {
        return lease::RefreshOutcome::Unavailable;
    };
    let cached_revision = lease::load_cached(vault, LEASE_VAULT_KEY)
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
        .map(|cached| cached.revision());
    match cached_revision {
        Some(cached) if cached >= claims.revision() => lease::RefreshOutcome::Current,
        _ => {
            let secret = Zeroizing::new(compact.as_bytes().to_vec());
            match vault.store(LEASE_VAULT_KEY, secret.as_slice()) {
                Ok(()) => lease::RefreshOutcome::Refreshed,
                Err(_) => lease::RefreshOutcome::Unavailable,
            }
        }
    }
}

fn cached_lease_covers<V>(
    service: &AccountService,
    session: &LeaseSession,
    vault: &V,
    now_unix_seconds: u64,
) -> bool
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
        .is_some_and(|claims| {
            claims.expires_at().saturating_sub(now_unix_seconds) <= LEASE_OFFLINE_VALIDITY_SECONDS
        })
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
    let Ok(tokens) = refresh_grant(
        &session.endpoints,
        &service.config.client_id,
        &session.refresh_token,
    ) else {
        return cached_outcome(service, &session, vault, now);
    };
    if let Some(rotated) = tokens.refresh.as_deref().filter(|token| !token.is_empty()) {
        let secret = Zeroizing::new(rotated.as_bytes().to_vec());
        let _ = vault.store(REFRESH_VAULT_KEY, secret.as_slice());
    }
    let Ok(compact) = lease::fetch_compact(
        &session.endpoints,
        &tokens.id_token,
        &tokens.subject,
        &session.device_id,
    ) else {
        return cached_outcome(service, &session, vault, now);
    };
    validate_and_cache(service, &session, &compact, vault, now)
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
    if cached_lease_covers(service, session, vault, now_unix_seconds) {
        lease::RefreshOutcome::OfflineCovered
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
        &tokens.id_token,
        &tokens.subject,
        &session.device_id,
    ) else {
        return lease::RefreshOutcome::Current;
    };
    // A failed initial fetch leaves the fresh Active session untouched.
    match validate_and_cache(service, &session, &compact, vault, unix_now()) {
        lease::RefreshOutcome::Unavailable => lease::RefreshOutcome::Current,
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
    // The newest valid entitlement lease uses its own account-specific key;
    // batch 1 records the lease slot without cached content until the
    // control plane issues signed leases.
    let _ = LEASE_VAULT_KEY;
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
    use super::{AccountService, AccountServiceConfig, LOGIN_TIMEOUT};
    use axiusflow_account::{AccountId, PlanId};
    use axiusflow_engine_protocol::AccountSessionState;
    use axiusflow_platform_runtime::CredentialVault;
    use std::{collections::HashMap, sync::Mutex};

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

    #[test]
    fn unconfigured_control_plane_fails_closed() {
        let service = AccountService::new(AccountServiceConfig {
            issuer: String::new(),
            client_id: "axiusflow-desktop".to_string(),
        });
        assert!(service.begin_login(1).is_err());
        assert_eq!(
            service.account_status().state,
            AccountSessionState::TerminalError as i32
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
            AccountSessionState::SignedOut as i32
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
            AccountSessionState::Authorizing as i32
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
        assert_eq!(view.state, AccountSessionState::SignedOut as i32);
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
        assert_eq!(view.state, AccountSessionState::Active as i32);
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
        assert_eq!(view.state, AccountSessionState::TerminalError as i32);
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
        assert_eq!(view.state, AccountSessionState::Active as i32);
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
        assert_eq!(
            service.account_status().state,
            AccountSessionState::Active as i32
        );
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
        assert_eq!(view.state, AccountSessionState::SignedOut as i32);
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
            AccountSessionState::SignedOut as i32
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
        assert_eq!(view.state, AccountSessionState::SignedOut as i32);
        assert!(view.display_name.is_empty());
    }
}
