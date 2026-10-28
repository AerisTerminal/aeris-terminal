#![cfg(test)]

use super::{
    AccountRestoreReadiness, AccountService, AccountServiceConfig, LOGIN_TIMEOUT, LocalRestore,
    OFFLINE_RETRY_INITIAL, OFFLINE_RETRY_MAXIMUM, OfflineBackoff, PROFILE_VAULT_KEY,
    REFRESH_VAULT_KEY, RejectedRestoreMarker, SESSION_REFRESH_INTERVAL,
    SESSION_REFRESH_JITTER_SECONDS, SessionRefresh, UnavailableVault, claim_profile_refresh,
    oidc::{self, AccountProfile, OidcEndpoints, SecretToken, VerifiedTokens},
    photo::PHOTO_RETRY_DELAY,
    reconcile_profile_photo, signed_in_view,
};
use crate::{AccountSessionState, AccountView, ProfilePhoto, ProfilePhotoStatus};
use aeris_account::AccountId;
use aeris_platform_runtime::CredentialVault;
use std::{
    collections::HashMap,
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::PathBuf,
    sync::{
        Arc, Barrier, Mutex, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

static MARKER_FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const ISSUER: &str = "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_fixture";
const HOSTED: &str = "https://auth.example.test";

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

/// Tries to commit a newer generation's refresh material while a stale
/// worker selects credentials, proving selection holds the vault commit fence.
struct GenerationRaceVault<'a> {
    inner: &'a MemoryVault,
    vault_commit: Arc<Mutex<()>>,
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
            if let Ok(_commit) = self.vault_commit.try_lock() {
                let mut state = self.state.lock().map_err(|_| "state".to_string())?;
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

/// Counts vault calls made while the account state lock is held.
struct StateProbeVault<'a> {
    inner: &'a MemoryVault,
    state: Arc<Mutex<super::ServiceState>>,
    calls: AtomicU64,
    calls_under_state_lock: AtomicU64,
}

impl StateProbeVault<'_> {
    fn observe(&self) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if matches!(self.state.try_lock(), Err(TryLockError::WouldBlock)) {
            self.calls_under_state_lock.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl CredentialVault for StateProbeVault<'_> {
    type Error = String;

    fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
        self.observe();
        self.inner.store(key, secret)
    }

    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        self.observe();
        self.inner.load(key)
    }

    fn delete(&self, key: &str) -> Result<(), Self::Error> {
        self.observe();
        self.inner.delete(key)
    }
}

/// Cancels the pending login from inside its profile write, as a user who
/// cancels while sign-in completes would.
struct CancellingVault<'a> {
    inner: &'a MemoryVault,
    service: &'a AccountService,
    generation: u64,
}

impl CredentialVault for CancellingVault<'_> {
    type Error = String;

    fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
        self.inner.store(key, secret)?;
        if key == PROFILE_VAULT_KEY {
            if self.service.state.try_lock().is_err() {
                return Err("vault write ran under the account state lock".to_string());
            }
            self.service.cancel_login(self.generation)?;
        }
        Ok(())
    }

    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        self.inner.load(key)
    }

    fn delete(&self, key: &str) -> Result<(), Self::Error> {
        self.inner.delete(key)
    }
}

fn fixture_endpoints() -> OidcEndpoints {
    OidcEndpoints {
        issuer: ISSUER.to_string(),
        authorization_endpoint: format!("{HOSTED}/oauth2/authorize"),
        token_endpoint: format!("{HOSTED}/oauth2/token"),
        jwks_uri: format!("{ISSUER}/.well-known/jwks.json"),
        revocation_endpoint: format!("{HOSTED}/oauth2/revoke"),
    }
}

fn service_with_marker_state(rejected_restore_marker: RejectedRestoreMarker) -> AccountService {
    let mut service = AccountService::new_with_rejected_restore_marker(
        AccountServiceConfig::try_new(ISSUER, HOSTED, "fixture-client")
            .expect("test config builds"),
        rejected_restore_marker,
    );
    // OS-assigned callback ports keep parallel tests off the registered ports.
    service.callback_ports = &[0];
    service.photo_fetcher = Arc::new(|_| Err("unit tests fetch no photos".to_string()));
    // Stub discovery so unit tests never touch the network.
    service
        .endpoints
        .lock()
        .expect("endpoint cache locks")
        .replace(fixture_endpoints());
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

/// Drops cached endpoints so sign-out starts no revocation worker.
fn without_revocation(service: &AccountService) {
    service
        .endpoints
        .lock()
        .expect("endpoint cache locks")
        .take();
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

fn account(value: &str) -> AccountId {
    AccountId::try_new(value).expect("identity builds")
}

fn profile(name: &str, email: &str) -> AccountProfile {
    AccountProfile {
        display_name: name.to_string(),
        email: email.to_string(),
        photo_url: format!("https://auth.example.test/photo/{name}.png"),
    }
}

fn verified(subject: &str, name: &str, refresh: Option<&str>) -> VerifiedTokens {
    VerifiedTokens {
        subject: subject.to_string(),
        profile: profile(name, &format!("{name}@example.test")),
        refresh: refresh.map(|token| SecretToken::new(token.to_string())),
    }
}

fn seed_saved_session(vault: &MemoryVault, subject: &str, name: &str) {
    vault
        .store(REFRESH_VAULT_KEY, b"saved-refresh")
        .expect("refresh fixture stores");
    super::store_profile_material(
        vault,
        &account(subject),
        &profile(name, &format!("{name}@example.test")),
    )
    .expect("profile fixture stores");
}

fn active_session(service: &AccountService, generation: u64, subject: &str) {
    let mut state = service.state.lock().expect("account state locks");
    state.last_generation = generation;
    state.restore_allowed = false;
    state.view = super::signed_in_view(
        AccountSessionState::Active,
        &account(subject),
        profile("ada", "ada@example.test"),
        generation,
        "signed in",
    );
}

#[test]
fn production_configuration_targets_the_aeris_user_pool() {
    let production = AccountServiceConfig::production();
    assert_eq!(
        production.issuer,
        "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_05pNG4EOF"
    );
    assert_eq!(production.hosted_origin, "https://auth.aeristerminal.com");
    assert_eq!(production.client_id, "tlnlg2amke4k2v9v2k84vvg87");
    assert!(AccountServiceConfig::try_new("http://issuer.test", HOSTED, "client").is_err());
    assert!(AccountServiceConfig::try_new(ISSUER, "http://auth.test", "client").is_err());
    assert!(AccountServiceConfig::try_new(ISSUER, HOSTED, " ").is_err());
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
fn unavailable_rejected_restore_marker_path_blocks_cached_restore() {
    let service = service_with_marker_state(RejectedRestoreMarker::Unavailable);
    service.set_restore_readiness(AccountRestoreReadiness::Pending);
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
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
fn saved_session_with_cached_profile_signs_in_offline_before_verification() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    assert!(matches!(
        service.restore_local_session(&vault),
        LocalRestore::ContinueOnline
    ));
    assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::Offline);
    assert_eq!(view.account_id, "acct_cached");
    assert_eq!(view.plan_id, "unlimited");
    assert_eq!(view.display_name, "ada");
    assert!(service.is_authenticated());
}

#[test]
fn saved_session_without_profile_waits_for_online_verification() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    vault
        .store(REFRESH_VAULT_KEY, b"saved-refresh")
        .expect("refresh fixture stores");
    assert!(matches!(
        service.restore_local_session(&vault),
        LocalRestore::ContinueOnline
    ));
    assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Ready);
    assert_eq!(
        service.account_status().state,
        AccountSessionState::Authorizing
    );
    assert!(!service.is_authenticated());
}

#[test]
fn unreadable_cached_profile_fails_readiness_but_restores_online() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    let failing = SelectiveLoadFailureVault {
        inner: &vault,
        failing_key: PROFILE_VAULT_KEY,
    };
    assert!(matches!(
        service.restore_local_session(&failing),
        LocalRestore::ContinueOnline
    ));
    assert_eq!(service.restore_readiness(), AccountRestoreReadiness::Failed);
    assert!(!service.is_authenticated());
}

#[test]
fn unreadable_refresh_material_fails_restore_closed() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
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
        AccountSessionState::TerminalError
    );
}

#[test]
fn malformed_refresh_material_requires_reauthentication_but_is_locally_ready() {
    for material in [&b""[..], &[0xff, 0xfe][..]] {
        let service = restoring_service();
        let vault = MemoryVault::default();
        vault
            .store(REFRESH_VAULT_KEY, material)
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
fn verified_online_restore_rotates_vault_material_and_refreshes_the_profile() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    assert!(matches!(
        service.restore_local_session(&vault),
        LocalRestore::ContinueOnline
    ));
    let tokens = service
        .accept_online_restore_refresh(
            &vault,
            Ok(verified(
                "acct_cached",
                "ada-renamed",
                Some("rotated-refresh"),
            )),
        )
        .expect("verified grant is accepted");
    assert_eq!(
        vault.load(REFRESH_VAULT_KEY).expect("refresh reads"),
        Some(b"rotated-refresh".to_vec()),
        "a rotated refresh token must be durable before the session activates"
    );
    assert!(service.apply_online_restore_tokens(&vault, &tokens));
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::Active);
    assert_eq!(view.display_name, "ada-renamed");
    let (cached_account, cached_profile) = super::load_profile_material(&vault)
        .expect("profile reads")
        .expect("profile is cached");
    assert_eq!(cached_account.as_str(), "acct_cached");
    assert_eq!(cached_profile.display_name, "ada-renamed");
}

#[test]
fn unrotated_refresh_keeps_the_saved_token() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    let _ = service.restore_local_session(&vault);
    let tokens = service
        .accept_online_restore_refresh(&vault, Ok(verified("acct_cached", "ada", None)))
        .expect("verified grant is accepted");
    assert!(service.apply_online_restore_tokens(&vault, &tokens));
    assert_eq!(
        vault.load(REFRESH_VAULT_KEY).expect("refresh reads"),
        Some(b"saved-refresh".to_vec())
    );
}

#[test]
fn unreachable_provider_keeps_a_cached_session_signed_in_for_retry() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    let _ = service.restore_local_session(&vault);
    assert!(
        service
            .accept_online_restore_refresh(&vault, Err(oidc::RefreshGrantError::Unavailable))
            .is_none()
    );
    assert!(service.hand_off_offline_restore());
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::Offline);
    assert_eq!(view.account_id, "acct_cached");
    assert!(service.is_authenticated());
    assert!(
        vault
            .load(REFRESH_VAULT_KEY)
            .expect("refresh reads")
            .is_some(),
        "unavailability is not expiry: the saved session stays restorable"
    );
}

#[test]
fn unreachable_provider_without_cached_profile_reports_without_discarding_the_session() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    vault
        .store(REFRESH_VAULT_KEY, b"saved-refresh")
        .expect("refresh fixture stores");
    let _ = service.restore_local_session(&vault);
    assert!(
        service
            .accept_online_restore_refresh(&vault, Err(oidc::RefreshGrantError::Unavailable))
            .is_none()
    );
    assert!(!service.hand_off_offline_restore());
    assert_eq!(
        service.account_status().state,
        AccountSessionState::TerminalError
    );
    assert!(
        vault
            .load(REFRESH_VAULT_KEY)
            .expect("refresh reads")
            .is_some()
    );
}

#[test]
fn rejected_online_restore_removes_every_restorable_session_credential() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    let _ = service.restore_local_session(&vault);
    assert!(service.is_authenticated());
    assert!(
        service
            .accept_online_restore_refresh(&vault, Err(oidc::RefreshGrantError::Rejected))
            .is_none()
    );
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::ReauthenticationRequired);
    assert_eq!(view.account_id, "");
    assert_eq!(view.display_name, "");
    assert!(!service.is_authenticated());
    assert!(!service.hand_off_offline_restore());
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
    assert!(vault.load(PROFILE_VAULT_KEY).expect("reads").is_none());
}

#[test]
fn rejected_restore_cleanup_failure_keeps_the_durable_marker_across_restart() {
    let (root, marker) = marker_fixture();
    let service = restoring_service_with_marker(Some(marker.clone()));
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    let _ = service.restore_local_session(&vault);
    let failing = SelectiveDeleteFailureVault {
        inner: &vault,
        failing_key: REFRESH_VAULT_KEY,
    };
    assert!(
        service
            .accept_online_restore_refresh(&failing, Err(oidc::RefreshGrantError::Rejected))
            .is_none()
    );
    assert_eq!(
        service.account_status().state,
        AccountSessionState::TerminalError
    );
    assert!(marker.is_file(), "partial cleanup keeps the restart fence");

    let restarted = restoring_service_with_marker(Some(marker.clone()));
    assert!(matches!(
        restarted.restore_local_session(&vault),
        LocalRestore::Settled
    ));
    assert!(!restarted.is_authenticated());

    restarted.begin_login(3).expect("fresh sign-in starts");
    assert!(restarted.complete_with_tokens(
        3,
        &account("acct_fresh"),
        &profile("fresh", "fresh@example.test"),
        "fresh-refresh",
        &vault,
    ));
    assert!(!marker.exists(), "a durable fresh sign-in clears the fence");
    fs::remove_dir_all(root).expect("marker fixture cleans up");
}

#[test]
fn stale_rejected_restore_cannot_delete_newer_session_credentials() {
    let service = service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_new", "new");
    {
        let mut state = service.state.lock().expect("account state locks");
        state.last_generation = 7;
        state.restore_allowed = false;
        state.view = super::cleared_view(AccountSessionState::SignedOut, 7, "signed out");
    }
    assert!(
        service
            .accept_online_restore_refresh(&vault, Err(oidc::RefreshGrantError::Rejected))
            .is_none()
    );
    assert_eq!(service.account_status().request_generation, 7);
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_some());
    assert!(vault.load(PROFILE_VAULT_KEY).expect("reads").is_some());
}

#[test]
fn online_restore_rotation_store_failure_leaves_nothing_restorable() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_cached", "ada");
    let _ = service.restore_local_session(&vault);
    let failing = SelectiveStoreFailureVault {
        inner: &vault,
        failing_key: REFRESH_VAULT_KEY,
    };
    assert!(
        service
            .accept_online_restore_refresh(
                &failing,
                Ok(verified("acct_cached", "ada", Some("rotated-refresh")))
            )
            .is_none()
    );
    assert_eq!(
        service.account_status().state,
        AccountSessionState::TerminalError
    );
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
    let restarted = restoring_service();
    assert!(matches!(
        restarted.restore_local_session(&vault),
        LocalRestore::Settled
    ));
    assert!(!restarted.is_authenticated());
}

#[test]
fn active_refresh_unavailability_goes_offline_and_verification_restores_active() {
    let service = service();
    active_session(&service, 12, "acct_01");
    super::apply_session_outcome(&service, 12, SessionRefresh::Unavailable);
    let offline = service.account_status();
    assert_eq!(offline.state, AccountSessionState::Offline);
    assert_eq!(offline.display_name, "ada");
    assert!(service.is_authenticated());

    super::apply_session_outcome(&service, 12, SessionRefresh::Verified);
    assert_eq!(service.account_status().state, AccountSessionState::Active);

    super::apply_session_outcome(&service, 11, SessionRefresh::Unavailable);
    assert_eq!(
        service.account_status().state,
        AccountSessionState::Active,
        "a retired generation cannot change the current session"
    );
}

#[test]
fn active_refresh_rejection_signs_out_and_retires_the_vault() {
    let service = service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_01", "ada");
    active_session(&service, 14, "acct_01");
    assert!(matches!(
        super::accept_refresh_grant(&service, 14, &vault, Err(oidc::RefreshGrantError::Rejected)),
        Err(SessionRefresh::Retired)
    ));
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::ReauthenticationRequired);
    assert_eq!(view.display_name, "");
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
    assert!(vault.load(PROFILE_VAULT_KEY).expect("reads").is_none());
}

#[test]
fn stale_refresh_rejection_cannot_retire_newer_session_credentials() {
    let service = service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_02", "bob");
    active_session(&service, 16, "acct_02");
    assert!(matches!(
        super::accept_refresh_grant(&service, 15, &vault, Err(oidc::RefreshGrantError::Rejected)),
        Err(SessionRefresh::Retired)
    ));
    assert_eq!(service.account_status().state, AccountSessionState::Active);
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_some());
}

#[test]
fn rotated_refresh_store_failure_fails_current_session_closed() {
    let service = service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_01", "ada");
    active_session(&service, 18, "acct_01");
    let failing = SelectiveStoreFailureVault {
        inner: &vault,
        failing_key: REFRESH_VAULT_KEY,
    };
    assert!(matches!(
        super::accept_refresh_grant(
            &service,
            18,
            &failing,
            Ok(verified("acct_01", "ada", Some("rotated")))
        ),
        Err(SessionRefresh::Retired)
    ));
    assert_eq!(
        service.account_status().state,
        AccountSessionState::TerminalError
    );
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
}

#[test]
fn session_credentials_bind_the_refresh_token_read_to_its_generation() {
    let service = service();
    let vault = MemoryVault::default();
    vault
        .store(REFRESH_VAULT_KEY, b"generation-refresh")
        .expect("refresh fixture stores");
    active_session(&service, 61, "acct_01");
    let racing = GenerationRaceVault {
        inner: &vault,
        vault_commit: Arc::clone(&service.vault_commit),
        state: Arc::clone(&service.state),
        attempted_advance: AtomicBool::new(false),
        advanced: AtomicBool::new(false),
        replacement_generation: 62,
    };

    let Ok(session) = super::session_credentials(&service, 61, &racing) else {
        panic!("session credentials build");
    };

    assert!(racing.attempted_advance.load(Ordering::Acquire));
    assert!(
        !racing.advanced.load(Ordering::Acquire),
        "a newer generation must not replace durable refresh material while stale credential selection owns the vault commit fence"
    );
    assert_eq!(session.refresh_token.as_str(), "generation-refresh");
    assert_eq!(session.account_id.as_str(), "acct_01");
    assert!(matches!(
        super::session_credentials(&service, 60, &vault),
        Err(SessionRefresh::Retired)
    ));
}

#[test]
fn vault_io_never_runs_under_the_account_state_lock() {
    let service = restoring_service();
    let vault = MemoryVault::default();
    seed_saved_session(&vault, "acct_01", "ada");
    let probe = StateProbeVault {
        inner: &vault,
        state: Arc::clone(&service.state),
        calls: AtomicU64::new(0),
        calls_under_state_lock: AtomicU64::new(0),
    };

    assert!(matches!(
        service.restore_local_session(&probe),
        LocalRestore::ContinueOnline
    ));
    assert!(
        service
            .current_restore_refresh_token(&probe)
            .expect("restore selection reads")
            .is_some()
    );
    let restored = service
        .accept_online_restore_refresh(
            &probe,
            Ok(verified("acct_01", "ada", Some("rotated-restore"))),
        )
        .expect("restore rotation commits");
    assert!(service.apply_online_restore_tokens(&probe, &restored));

    let Ok(session) = super::session_credentials(&service, 0, &probe) else {
        panic!("restored session selects its credentials");
    };
    assert_eq!(session.refresh_token.as_str(), "rotated-restore");
    let Ok(refreshed) = super::accept_refresh_grant(
        &service,
        0,
        &probe,
        Ok(verified("acct_01", "ada-renamed", Some("rotated-session"))),
    ) else {
        panic!("session rotation commits");
    };
    assert!(service.apply_verified_profile(&probe, 0, &account("acct_01"), &refreshed));
    service.retire_rejected_session(0, &probe);
    assert_eq!(
        service.account_status().state,
        AccountSessionState::ReauthenticationRequired
    );

    service.begin_login(5).expect("login starts");
    assert!(service.complete_with_tokens(
        5,
        &account("acct_02"),
        &profile("bob", "bob@example.test"),
        "refresh-bob",
        &probe,
    ));
    without_revocation(&service);
    service
        .sign_out_with(&probe)
        .expect("account signs out durably");

    assert!(probe.calls.load(Ordering::Acquire) >= 12);
    assert_eq!(
        probe.calls_under_state_lock.load(Ordering::Acquire),
        0,
        "native vault I/O must never block account state readers"
    );
}

#[test]
fn login_cancelled_during_its_vault_commit_leaves_nothing_restorable() {
    let service = service();
    let vault = MemoryVault::default();
    service.begin_login(51).expect("login starts");
    let cancelling = CancellingVault {
        inner: &vault,
        service: &service,
        generation: 51,
    };

    assert!(!service.complete_with_tokens(
        51,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-cancelled",
        &cancelling,
    ));
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::SignedOut);
    assert_eq!(view.detail, "sign-in cancelled");
    assert_eq!(view.account_id, "");
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
    assert!(vault.load(PROFILE_VAULT_KEY).expect("reads").is_none());
}

#[test]
fn profile_cache_beyond_the_vault_limit_is_rejected_before_the_vault() {
    let vault = MemoryVault::default();
    let bounded = AccountProfile {
        display_name: "n".repeat(aeris_account::MAXIMUM_PROFILE_NAME_BYTES),
        email: "e".repeat(aeris_account::MAXIMUM_PROFILE_EMAIL_BYTES),
        photo_url: "https://auth.example.test/photo/ada.png".to_string(),
    };
    super::store_profile_material(&vault, &account("acct_01"), &bounded)
        .expect("a profile with maximum name and email fits the vault");
    let oversized = AccountProfile {
        photo_url: "p".repeat(aeris_account::MAXIMUM_PROFILE_PHOTO_URL_BYTES),
        ..bounded
    };
    assert!(super::store_profile_material(&vault, &account("acct_01"), &oversized).is_err());
    let cached = vault
        .load(PROFILE_VAULT_KEY)
        .expect("reads")
        .expect("the earlier profile remains");
    assert!(cached.len() <= aeris_platform_runtime::MAX_SECRET_BYTES);
}

#[test]
fn missing_refresh_material_fails_an_active_session_closed() {
    let service = service();
    let vault = MemoryVault::default();
    active_session(&service, 63, "acct_01");
    assert!(matches!(
        super::session_credentials(&service, 63, &vault),
        Err(SessionRefresh::Retired)
    ));
    assert_eq!(
        service.account_status().state,
        AccountSessionState::TerminalError
    );
}

#[test]
fn verified_profile_refresh_is_account_and_generation_fenced() {
    let service = service();
    let vault = MemoryVault::default();
    active_session(&service, 21, "acct_01");
    let renamed = verified("acct_01", "ada-renamed", None);
    assert!(!service.apply_verified_profile(&vault, 20, &account("acct_01"), &renamed));
    assert!(!service.apply_verified_profile(&vault, 21, &account("acct_02"), &renamed));
    assert!(!service.apply_verified_profile(
        &vault,
        21,
        &account("acct_01"),
        &verified("acct_02", "bob", None)
    ));
    assert_eq!(service.account_status().display_name, "ada");
    assert!(service.apply_verified_profile(&vault, 21, &account("acct_01"), &renamed));
    let view = service.account_status();
    assert_eq!(view.display_name, "ada-renamed");
    assert_eq!(view.email, "ada-renamed@example.test");
    assert!(
        super::load_profile_material(&vault)
            .expect("profile reads")
            .is_some()
    );
}

#[test]
fn profile_refresh_claims_only_one_current_signed_in_request() {
    let in_flight = AtomicBool::new(false);
    let signed_out = super::cleared_view(AccountSessionState::SignedOut, 3, "signed out");
    assert_eq!(claim_profile_refresh(&signed_out, &in_flight), None);
    let mut signed_in = super::signed_in_view(
        AccountSessionState::Offline,
        &account("acct_01"),
        AccountProfile::default(),
        3,
        "signed in",
    );
    assert_eq!(claim_profile_refresh(&signed_in, &in_flight), Some(3));
    signed_in.state = AccountSessionState::Active;
    assert_eq!(claim_profile_refresh(&signed_in, &in_flight), None);
    in_flight.store(false, Ordering::Release);
    assert_eq!(claim_profile_refresh(&signed_in, &in_flight), Some(3));
}

#[test]
fn offline_retries_back_off_within_their_bound() {
    let mut backoff = OfflineBackoff::new();
    let delays: Vec<Duration> = (0..8).map(|_| backoff.next_delay()).collect();
    assert_eq!(delays[0], OFFLINE_RETRY_INITIAL);
    assert_eq!(delays[1], OFFLINE_RETRY_INITIAL * 2);
    assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(delays[7], OFFLINE_RETRY_MAXIMUM);
    assert_eq!(super::session_refresh_wait(None), SESSION_REFRESH_INTERVAL);
    let jittered = super::session_refresh_wait(Some([0xff; 8]));
    assert!(
        jittered < SESSION_REFRESH_INTERVAL + Duration::from_secs(SESSION_REFRESH_JITTER_SECONDS)
    );
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
        LocalRestore::ContinueOnline
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
    active_session(&service, 9, "acct_01");
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

#[test]
fn duplicate_begin_and_retired_generations_fail_closed() {
    let service = service();
    let authorization = service.begin_login(1).expect("first login starts");
    assert!(
        authorization
            .authorization_url
            .contains("redirect_uri=http%3A%2F%2Flocalhost%3A")
    );
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
        .strip_prefix("http://localhost:")
        .and_then(|value| value.strip_suffix("/callback"))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("loopback redirect carries its bound port");

    service.cancel_login(5).expect("cancel succeeds");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))) {
            Ok(listener) => {
                drop(listener);
                break;
            }
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
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
    assert!(!service.complete_with_tokens(
        9,
        &account("acct_01"),
        &profile("stale", "stale@example.com"),
        "refresh",
        &vault,
    ));
    // A retired callback restores nothing: neither state nor another
    // user's profile may leak into the current session.
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::SignedOut);
    assert_eq!(view.display_name, "");
    assert_eq!(view.email, "");
    assert_eq!(view.photo_url, "");
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
}

#[test]
fn active_completion_stores_refresh_and_profile_material() {
    let service = service();
    service.begin_login(11).expect("login starts");
    let vault = MemoryVault::default();
    assert!(service.complete_with_tokens(
        11,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-value",
        &vault,
    ));
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::Active);
    assert_eq!(view.account_id, "acct_01");
    assert_eq!(view.plan_id, "unlimited");
    assert_eq!(view.display_name, "ada");
    assert_eq!(view.email, "ada@example.com");
    assert_eq!(view.photo_url, "https://auth.example.test/photo/ada.png");
    assert_eq!(
        vault.load(REFRESH_VAULT_KEY).expect("reads"),
        Some(b"refresh-value".to_vec())
    );
    assert!(vault.load(PROFILE_VAULT_KEY).expect("reads").is_some());
}

#[test]
fn vault_failure_reports_without_activating_or_leaking_profile() {
    let service = service();
    service.begin_login(12).expect("login starts");
    assert!(!service.complete_with_tokens(
        12,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-value",
        &UnavailableVault,
    ));
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::TerminalError);
    assert_eq!(view.account_id, "");
    assert_eq!(view.display_name, "");
    assert_eq!(view.email, "");
    assert_eq!(view.photo_url, "");
}

#[test]
fn account_switch_replaces_profile_without_carryover() {
    let service = service();
    let vault = MemoryVault::default();
    service.begin_login(31).expect("first login starts");
    assert!(service.complete_with_tokens(
        31,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-ada",
        &vault,
    ));
    assert_eq!(service.account_status().display_name, "ada");
    without_revocation(&service);
    let signed_out = service
        .sign_out_with(&vault)
        .expect("first account signs out durably");
    assert_eq!(signed_out.display_name, "");
    assert_eq!(signed_out.email, "");
    assert_eq!(signed_out.photo_url, "");
    service
        .endpoints
        .lock()
        .expect("endpoint cache locks")
        .replace(fixture_endpoints());
    service.begin_login(32).expect("second login starts");
    assert!(service.complete_with_tokens(
        32,
        &account("acct_02"),
        &profile("bob", "bob@example.com"),
        "refresh-bob",
        &vault,
    ));
    let view = service.account_status();
    assert_eq!(view.state, AccountSessionState::Active);
    assert_eq!(view.account_id, "acct_02");
    assert_eq!(view.display_name, "bob");
    assert_eq!(view.email, "bob@example.com");
    assert!(view.photo_url.contains("bob"));
}

fn photo_fixture() -> ProfilePhoto {
    ProfilePhoto::from_bytes(b"GIF89a\x01\0\x01\0".to_vec()).expect("photo fixture builds")
}

fn signed_in_with_photo(service: &AccountService, name: &str) {
    service.state.lock().expect("account state locks").view = signed_in_view(
        AccountSessionState::Active,
        &account("acct_01"),
        profile(name, &format!("{name}@example.test")),
        1,
        "signed in",
    );
}

#[test]
fn profile_photo_loads_once_for_the_signed_in_address_and_clears_on_sign_out() {
    let mut service = service();
    let fetched = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = Arc::clone(&fetched);
    service.photo_fetcher = Arc::new(move |url| {
        recorder
            .lock()
            .expect("fetch record locks")
            .push(url.to_string());
        Ok(photo_fixture())
    });
    assert_eq!(service.account_status().photo, ProfilePhotoStatus::Absent);
    let vault = MemoryVault::default();
    service.begin_login(41).expect("login starts");
    assert!(service.complete_with_tokens(
        41,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-ada",
        &vault,
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while service.account_status().photo != ProfilePhotoStatus::Loaded(photo_fixture()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the profile photo must load"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..3 {
        assert_eq!(
            service.account_status().photo,
            ProfilePhotoStatus::Loaded(photo_fixture())
        );
    }
    assert_eq!(
        *fetched.lock().expect("fetch record locks"),
        vec!["https://auth.example.test/photo/ada.png".to_string()]
    );

    without_revocation(&service);
    let signed_out = service.sign_out_with(&vault).expect("sign-out succeeds");
    assert_eq!(signed_out.photo, ProfilePhotoStatus::Absent);
    assert_eq!(service.account_status().photo, ProfilePhotoStatus::Absent);
}

#[test]
fn a_replaced_photo_address_drops_the_previous_fetch() {
    let service = service();
    signed_in_with_photo(&service, "ada");
    let now = std::time::Instant::now();
    let first = {
        let mut state = service.state.lock().expect("account state locks");
        let first = reconcile_profile_photo(&mut state, now).expect("first photo fetch is due");
        assert_eq!(state.view.photo, ProfilePhotoStatus::Loading);
        assert!(reconcile_profile_photo(&mut state, now).is_none());
        first
    };
    signed_in_with_photo(&service, "bob");
    let second = reconcile_profile_photo(&mut service.state.lock().expect("state locks"), now)
        .expect("the replaced address starts its own fetch");
    assert_eq!(second.url, "https://auth.example.test/photo/bob.png");

    service.complete_photo_fetch(first.sequence, Ok(photo_fixture()));
    assert_eq!(service.account_status().photo, ProfilePhotoStatus::Loading);
    service.complete_photo_fetch(second.sequence, Ok(photo_fixture()));
    assert_eq!(
        service.account_status().photo,
        ProfilePhotoStatus::Loaded(photo_fixture())
    );
}

#[test]
fn a_failed_photo_fetch_waits_out_the_retry_delay() {
    let service = service();
    signed_in_with_photo(&service, "ada");
    let now = std::time::Instant::now();
    let fetch = reconcile_profile_photo(&mut service.state.lock().expect("state locks"), now)
        .expect("photo fetch is due");
    service.complete_photo_fetch(fetch.sequence, Err("unreachable".to_string()));
    let mut state = service.state.lock().expect("account state locks");
    assert!(reconcile_profile_photo(&mut state, std::time::Instant::now()).is_none());
    assert_eq!(state.view.photo, ProfilePhotoStatus::Absent);
    assert!(
        reconcile_profile_photo(&mut state, std::time::Instant::now() + PHOTO_RETRY_DELAY)
            .is_some()
    );
}

#[test]
fn signed_out_sessions_fetch_no_photo() {
    let service = service();
    signed_in_with_photo(&service, "ada");
    let mut state = service.state.lock().expect("account state locks");
    state.view.state = AccountSessionState::ReauthenticationRequired;
    assert!(reconcile_profile_photo(&mut state, std::time::Instant::now()).is_none());
    assert_eq!(state.view.photo, ProfilePhotoStatus::Absent);
}

#[test]
fn login_timeout_is_configured() {
    assert!(LOGIN_TIMEOUT.as_secs() >= 60);
}

#[test]
fn sign_out_clears_state_and_deletes_vault_material() {
    let service = service();
    let vault = MemoryVault::default();
    service.begin_login(21).expect("login starts");
    assert!(service.complete_with_tokens(
        21,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-value",
        &vault,
    ));
    without_revocation(&service);
    let view = service
        .sign_out_with(&vault)
        .expect("account signs out durably");
    assert_eq!(view.state, AccountSessionState::SignedOut);
    assert_eq!(view.account_id, "");
    assert_eq!(view.display_name, "");
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_none());
    assert!(vault.load(PROFILE_VAULT_KEY).expect("reads").is_none());
    assert!(!service.is_authenticated());
    // A retired completion after sign-out cannot resurrect the session.
    assert!(!service.complete_with_tokens(
        21,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-value",
        &vault,
    ));
    assert_eq!(
        service.account_status().state,
        AccountSessionState::SignedOut
    );
}

#[test]
fn sign_out_refresh_delete_failure_is_actionable_and_survives_restart() {
    let service = service();
    let vault = MemoryVault::default();
    service.begin_login(41).expect("login starts");
    assert!(service.complete_with_tokens(
        41,
        &account("acct_01"),
        &profile("ada", "ada@example.com"),
        "refresh-survives",
        &vault,
    ));
    without_revocation(&service);
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
    assert_eq!(signed_out.account_id, "");
    assert!(vault.load(REFRESH_VAULT_KEY).expect("reads").is_some());

    let restarted = restoring_service();
    assert!(matches!(
        restarted.restore_local_session(&vault),
        LocalRestore::ContinueOnline
    ));
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
fn signed_in_views_carry_the_unlimited_plan_and_cleared_views_carry_nothing() {
    let signed_in = super::signed_in_view(
        AccountSessionState::Active,
        &account("acct_01"),
        profile("ada", "ada@example.test"),
        4,
        "signed in",
    );
    assert_eq!(signed_in.plan_id, "unlimited");
    let cleared: AccountView = super::cleared_view(AccountSessionState::SignedOut, 4, "signed out");
    assert_eq!(cleared.plan_id, "");
    assert_eq!(cleared.account_id, "");
}
