#![cfg(test)]

use super::{
    AccountRestoreReadiness, AccountService, AccountServiceConfig, DEVICE_VAULT_KEY,
    LEASE_DIRECTORY_VAULT_KEY, LEASE_VAULT_KEY, LOGIN_TIMEOUT, LocalRestore, REFRESH_VAULT_KEY,
    RejectedRestoreMarker, UnavailableVault, claim_profile_refresh, configured_auth_issuer,
    oidc::{self, AccountProfile, VerifiedTokens},
};
use aeris_account::{AccountId, PlanId};
use aeris_contracts::{AccountSessionState, AccountView};
use aeris_platform_runtime::CredentialVault;
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

#[test]
fn auth_issuer_uses_aeris_override_or_remains_unconfigured() {
    let preferred = configured_auth_issuer(|name| match name {
        "AERIS_AUTH_ISSUER" => Some("https://preferred.example/api/auth".to_string()),
        "AXIUSFLOW_AUTH_ISSUER" => Some("https://legacy.example/api/auth".to_string()),
        _ => None,
    });
    assert_eq!(preferred, "https://preferred.example/api/auth");

    let legacy = configured_auth_issuer(|name| match name {
        "AXIUSFLOW_AUTH_ISSUER" => Some("https://legacy.example/api/auth".to_string()),
        _ => None,
    });
    assert!(legacy.is_empty());

    assert_eq!(configured_auth_issuer(|_| None), super::DEFAULT_AUTH_ISSUER);
}

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
        AccountServiceConfig::try_new("https://auth.example.test/api/auth", "aeris-desktop")
            .expect("test config builds"),
        rejected_restore_marker,
    );
    // Stub discovery so unit tests never touch the network.
    service
        .endpoints
        .lock()
        .expect("endpoint cache locks")
        .replace(OidcEndpoints {
            issuer: "https://auth.example.test/api/auth".to_string(),
            authorization_endpoint: "https://auth.example.test/api/auth/oauth2/authorize"
                .to_string(),
            token_endpoint: "https://auth.example.test/api/auth/oauth2/token".to_string(),
            jwks_uri: "https://auth.example.test/api/auth/jwks".to_string(),
            revocation_endpoint: "https://auth.example.test/api/auth/oauth2/revoke".to_string(),
            link_endpoint: "https://auth.example.test/api/aeris/link".to_string(),
            lease_endpoint: "https://auth.example.test/api/aeris/lease".to_string(),
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
        "aeris-account-restore-marker-{}-{sequence}",
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
        "aud": "aeris-desktop",
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
            let session =
                super::lease_session(&service, 61, &vault).expect("fresh active session builds");
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
                state.view = super::cleared_view(AccountSessionState::SignedOut, 7, "signed out");
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
        client_id: "aeris-desktop".to_string(),
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
        client_id: "aeris-desktop".to_string(),
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
        client_id: "aeris-desktop".to_string(),
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
        photo_url: format!("https://auth.example.test/photo/{name}.png"),
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
    assert_eq!(view.photo_url, "https://auth.example.test/photo/ada.png");
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
    let fixture_endpoints = service
        .endpoints
        .lock()
        .expect("endpoint cache locks")
        .take()
        .expect("fixture endpoints are cached");
    let signed_out = service
        .sign_out_with(&vault)
        .expect("first account signs out durably");
    assert!(signed_out.display_name.is_empty());
    assert!(signed_out.email.is_empty());
    assert!(signed_out.photo_url.is_empty());
    service
        .endpoints
        .lock()
        .expect("endpoint cache locks")
        .replace(fixture_endpoints);
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
    use aeris_platform_runtime::CredentialVault as _;

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
