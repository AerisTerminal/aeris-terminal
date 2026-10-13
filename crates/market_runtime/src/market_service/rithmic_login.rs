//! Runtime-owned Rithmic login: the server catalog, system discovery, and the
//! single saved login in the native credential vault.
//!
//! The vault is the only credential boundary. Callers hand the user ID and
//! password over once to be saved; nothing here returns them again.

use super::MarketService;
use crate::rithmic_realtime::RithmicLoginRevision;
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use aeris_rithmic_protocol_adapter::{
    MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES, RITHMIC_LOGIN_VAULT_KEY, RITHMIC_VAULT_SERVICE,
    RithmicCredentialBytes, RithmicCredentialError, RithmicCredentials, RithmicGateway,
    RithmicSession, RithmicSessionLimits,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

/// Most systems one discovery answer may list for selection.
const MAXIMUM_DISCOVERED_SYSTEMS: usize = 64;

/// One Rithmic server (R | Protocol connect point) a login can sign in through.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RithmicServer(RithmicGateway);

impl RithmicServer {
    /// The core Chicago server, where a new login starts.
    pub const CORE: Self = Self(RithmicGateway::Chicago);

    /// Every server, the core Chicago server first.
    pub fn all() -> impl Iterator<Item = Self> {
        RithmicGateway::ALL.into_iter().map(Self)
    }

    /// The name shown when choosing a server.
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.0.label()
    }

    #[must_use]
    pub const fn host(self) -> &'static str {
        self.0.host()
    }
}

/// Non-secret description of the saved login. The user ID and password stay
/// in the vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicLoginSummary {
    pub server: RithmicServer,
    pub system_name: String,
}

pub(super) struct RithmicLogin {
    revision: RithmicLoginRevision,
    discovering: AtomicBool,
}

impl RithmicLogin {
    pub(super) const fn new(revision: RithmicLoginRevision) -> Self {
        Self {
            revision,
            discovering: AtomicBool::new(false),
        }
    }
}

impl MarketService {
    /// Lists the systems one Rithmic server offers, such as
    /// "Rithmic Paper Trading". Discovery needs no credentials. Call from a
    /// background worker: this opens one bounded WSS connection, and only one
    /// discovery runs at a time.
    ///
    /// # Errors
    /// Rejects a concurrent discovery, or reports a redacted connection or
    /// protocol failure.
    pub fn discover_rithmic_systems(&self, server: RithmicServer) -> Result<Vec<String>, String> {
        let discovering = &self.runtime.rithmic_login.discovering;
        discovering
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Rithmic system discovery is already running".to_string())?;
        let result = RithmicSession::discover_systems(
            server.0,
            RithmicSessionLimits::default(),
            Some(Arc::clone(&self.runtime.shutdown)),
        );
        discovering.store(false, Ordering::Release);
        result
            .map(selectable_systems)
            .map_err(|error| format!("Rithmic system discovery failed: {error}"))
    }

    /// Describes the saved Rithmic login without exposing its credentials.
    /// Call from a background worker; this reads the native credential vault.
    ///
    /// # Errors
    /// Reports an unavailable vault or a saved login that can no longer be read.
    pub fn saved_rithmic_login(&self) -> Result<Option<RithmicLoginSummary>, String> {
        load_summary(&native_vault()?)
    }

    /// Saves the Rithmic login to the native credential vault, replacing any
    /// earlier one, and retires sessions signed in under the earlier login so
    /// the next connection uses this one. Call from a background worker.
    ///
    /// # Errors
    /// Rejects empty, oversized or control-containing fields, or reports an
    /// unavailable vault.
    pub fn save_rithmic_login(
        &self,
        server: RithmicServer,
        system_name: &str,
        user: &str,
        password: &str,
    ) -> Result<(), String> {
        store_login(&native_vault()?, server, system_name, user, password)?;
        self.runtime.rithmic_login.revision.advance();
        Ok(())
    }

    /// Deletes the saved Rithmic login and retires sessions signed in with it.
    /// Call from a background worker.
    ///
    /// # Errors
    /// Reports an unavailable vault or a failed deletion.
    pub fn forget_rithmic_login(&self) -> Result<(), String> {
        native_vault()?
            .delete(RITHMIC_LOGIN_VAULT_KEY)
            .map_err(|_| "Saved Rithmic login could not be deleted".to_string())?;
        self.runtime.rithmic_login.revision.advance();
        Ok(())
    }
}

fn native_vault() -> Result<NativeCredentialVault, String> {
    NativeCredentialVault::new(RITHMIC_VAULT_SERVICE)
        .map_err(|_| "Protected credential storage is unavailable".to_string())
}

fn selectable_systems(names: Vec<String>) -> Vec<String> {
    let mut systems = Vec::new();
    for name in names {
        if systems.len() == MAXIMUM_DISCOVERED_SYSTEMS {
            break;
        }
        let selectable = !name.trim().is_empty()
            && name.len() <= MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES
            && !name.chars().any(char::is_control);
        if selectable && !systems.contains(&name) {
            systems.push(name);
        }
    }
    systems
}

fn store_login(
    vault: &impl CredentialVault,
    server: RithmicServer,
    system_name: &str,
    user: &str,
    password: &str,
) -> Result<(), String> {
    let encoded = RithmicCredentialBytes::try_encode(RithmicCredentials {
        gateway: server.0,
        system_name,
        user,
        password,
    })
    .map_err(login_field_error)?;
    vault
        .store(RITHMIC_LOGIN_VAULT_KEY, encoded.as_bytes())
        .map_err(|_| "Rithmic login could not be saved to protected storage".to_string())
}

fn load_summary(vault: &impl CredentialVault) -> Result<Option<RithmicLoginSummary>, String> {
    let Some(stored) = vault
        .load(RITHMIC_LOGIN_VAULT_KEY)
        .map_err(|_| "Saved Rithmic login could not be loaded".to_string())?
    else {
        return Ok(None);
    };
    let stored = Zeroizing::new(stored);
    let copied = RithmicCredentialBytes::try_copy_from_vault(&stored)
        .map_err(|_| "Saved Rithmic login is unreadable; save it again".to_string())?;
    let credentials = copied
        .credentials()
        .map_err(|_| "Saved Rithmic login is unreadable; save it again".to_string())?;
    Ok(Some(RithmicLoginSummary {
        server: RithmicServer(credentials.gateway),
        system_name: credentials.system_name.to_string(),
    }))
}

fn login_field_error(error: RithmicCredentialError) -> String {
    match error {
        RithmicCredentialError::SystemEmpty => "Choose a Rithmic system",
        RithmicCredentialError::UserEmpty => "Enter the Rithmic user ID",
        RithmicCredentialError::PasswordEmpty => "Enter the Rithmic password",
        RithmicCredentialError::SystemTooLong { .. }
        | RithmicCredentialError::SystemInvalidUtf8
        | RithmicCredentialError::SystemControlCharacter => "The Rithmic system name is invalid",
        RithmicCredentialError::UserTooLong { .. }
        | RithmicCredentialError::UserInvalidUtf8
        | RithmicCredentialError::UserControlCharacter => "The Rithmic user ID is invalid",
        RithmicCredentialError::PasswordTooLong { .. }
        | RithmicCredentialError::PasswordInvalidUtf8
        | RithmicCredentialError::PasswordControlCharacter => "The Rithmic password is invalid",
        RithmicCredentialError::BlobTooLarge { .. }
        | RithmicCredentialError::Truncated
        | RithmicCredentialError::UnsupportedVersion
        | RithmicCredentialError::UnknownGateway
        | RithmicCredentialError::LengthMismatch => "The Rithmic login is invalid",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryVault(Mutex<Option<Vec<u8>>>);

    impl CredentialVault for MemoryVault {
        type Error = ();
        fn store(&self, key: &str, bytes: &[u8]) -> Result<(), ()> {
            assert_eq!(key, RITHMIC_LOGIN_VAULT_KEY);
            *self.0.lock().unwrap() = Some(bytes.to_vec());
            Ok(())
        }
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ()> {
            assert_eq!(key, RITHMIC_LOGIN_VAULT_KEY);
            Ok(self.0.lock().unwrap().clone())
        }
        fn delete(&self, key: &str) -> Result<(), ()> {
            assert_eq!(key, RITHMIC_LOGIN_VAULT_KEY);
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn chicago() -> RithmicServer {
        RithmicServer::all()
            .find(|server| server.host() == "rprotocol.rithmic.com")
            .expect("Chicago server is listed")
    }

    #[test]
    fn saved_login_summary_names_server_and_system_but_never_credentials() {
        let vault = MemoryVault::default();
        assert_eq!(load_summary(&vault), Ok(None));
        store_login(
            &vault,
            chicago(),
            "Rithmic Paper Trading",
            "fixture-user",
            "fixture-password",
        )
        .expect("login saves");
        let summary = load_summary(&vault)
            .expect("summary loads")
            .expect("login is saved");
        assert_eq!(summary.server, chicago());
        assert_eq!(summary.system_name, "Rithmic Paper Trading");
        let debug = format!("{summary:?}");
        assert!(!debug.contains("fixture-user"));
        assert!(!debug.contains("fixture-password"));
    }

    #[test]
    fn invalid_login_fields_are_rejected_before_storage() {
        let vault = MemoryVault::default();
        for (system, user, password, expected) in [
            ("", "user", "password", "Choose a Rithmic system"),
            ("Rithmic 01", "", "password", "Enter the Rithmic user ID"),
            ("Rithmic 01", "user", "", "Enter the Rithmic password"),
            (
                "Rithmic 01",
                "us\ner",
                "password",
                "The Rithmic user ID is invalid",
            ),
        ] {
            assert_eq!(
                store_login(&vault, chicago(), system, user, password),
                Err(expected.to_string())
            );
        }
        assert_eq!(load_summary(&vault), Ok(None));
    }

    #[test]
    fn unreadable_saved_login_asks_to_save_again() {
        let vault = MemoryVault::default();
        vault
            .store(RITHMIC_LOGIN_VAULT_KEY, &[1, 0, 1, 0, 1, b'u', b'p'])
            .unwrap();
        assert_eq!(
            load_summary(&vault),
            Err("Saved Rithmic login is unreadable; save it again".to_string())
        );
    }

    #[test]
    fn discovered_systems_are_bounded_unique_and_selectable() {
        let mut names = vec![
            "Rithmic Paper Trading".to_string(),
            String::new(),
            "Rithmic Paper Trading".to_string(),
            "bad\u{7}name".to_string(),
            "x".repeat(MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES + 1),
            "Rithmic 01".to_string(),
        ];
        names.extend((0..100).map(|index| format!("System {index}")));
        let systems = selectable_systems(names);
        assert_eq!(systems.len(), MAXIMUM_DISCOVERED_SYSTEMS);
        assert_eq!(systems[..2], ["Rithmic Paper Trading", "Rithmic 01"]);
    }

    #[test]
    fn every_server_is_listed_once_with_a_label() {
        let servers = RithmicServer::all().collect::<Vec<_>>();
        assert_eq!(servers.first(), Some(&chicago()));
        assert_eq!(RithmicServer::CORE, chicago());
        for server in &servers {
            let named_alike = servers
                .iter()
                .filter(|other| other.label() == server.label())
                .count();
            assert_eq!(named_alike, 1, "{server:?} needs its own label");
        }
    }
}
