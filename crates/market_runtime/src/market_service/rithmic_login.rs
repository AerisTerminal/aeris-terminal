//! Runtime-owned Rithmic login: the server catalog, system discovery, and the
//! single saved login in the native credential vault.
//!
//! The vault is the only credential boundary. Callers hand the user ID and
//! password over once to be signed in and saved; nothing here returns them again.

use super::{
    MarketService,
    descriptors::{
        LoginField, LoginFieldValue, LoginSchema, ProviderLoginServer, SavedProviderLogin,
    },
};
use crate::rithmic_realtime::RithmicLoginRevision;
use aeris_observability::diagnostic;
use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use aeris_rithmic_protocol_adapter::{
    MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES, RITHMIC_APPLICATION_NAME, RITHMIC_LOGIN_VAULT_KEY,
    RITHMIC_VAULT_SERVICE, RithmicApplication, RithmicCredentialBytes, RithmicCredentialError,
    RithmicCredentials, RithmicGateway, RithmicSession, RithmicSessionError, RithmicSessionLimits,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

/// Most systems one discovery answer may list for selection.
const MAXIMUM_DISCOVERED_SYSTEMS: usize = 64;

pub(super) const LOGIN_SCHEMA: LoginSchema = LoginSchema {
    fields: &[
        LoginField {
            id: "server",
            label: "Server",
            secret: false,
        },
        LoginField {
            id: "system",
            label: "System",
            secret: false,
        },
        LoginField {
            id: "user",
            label: "User ID",
            secret: false,
        },
        LoginField {
            id: "password",
            label: "Password",
            secret: true,
        },
    ],
    discover_servers: true,
};

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

    fn from_id(id: &str) -> Option<Self> {
        Self::all().find(|server| server.host() == id)
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
    pub(super) revision: RithmicLoginRevision,
    discovering: AtomicBool,
    signing_in: AtomicBool,
    /// Whether the broker venue relay has started; it may start once.
    pub(super) venue_attached: AtomicBool,
}

impl RithmicLogin {
    pub(super) const fn new(revision: RithmicLoginRevision) -> Self {
        Self {
            revision,
            discovering: AtomicBool::new(false),
            signing_in: AtomicBool::new(false),
            venue_attached: AtomicBool::new(false),
        }
    }
}

impl MarketService {
    fn credential_provider(provider_id: &str) -> Result<(), String> {
        let registration = super::BUILT_IN_PROVIDER_DESCRIPTORS
            .iter()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| "Unsupported provider".to_string())?;
        if registration.login_schema.is_none() {
            return Err("This provider does not use saved credentials".to_string());
        }
        if registration.id != super::RITHMIC_DESCRIPTOR.id {
            return Err("Provider login is unavailable".to_string());
        }
        Ok(())
    }

    /// The non-secret servers offered by a registered vault login provider.
    ///
    /// # Errors
    /// Rejects unknown or public providers.
    pub fn provider_login_servers(
        &self,
        provider_id: &str,
    ) -> Result<Vec<ProviderLoginServer>, String> {
        Self::credential_provider(provider_id)?;
        Ok(RithmicServer::all()
            .map(|server| ProviderLoginServer {
                id: server.host(),
                label: server.label(),
            })
            .collect())
    }

    /// Discovers the systems at a registered provider's server. Run off the UI thread.
    ///
    /// # Errors
    /// Rejects unsupported servers and reports redacted discovery failures.
    pub fn discover_provider_servers(
        &self,
        provider_id: &str,
        server_id: &str,
    ) -> Result<Vec<String>, String> {
        Self::credential_provider(provider_id)?;
        let server = RithmicServer::from_id(server_id)
            .ok_or_else(|| "Unknown provider server".to_string())?;
        self.discover_rithmic_systems(server)
    }

    /// Reads only non-secret saved-login metadata from the provider's native vault entry.
    ///
    /// # Errors
    /// Rejects unsupported providers or unreadable vault entries.
    pub fn saved_provider_login(
        &self,
        provider_id: &str,
    ) -> Result<Option<SavedProviderLogin>, String> {
        Self::credential_provider(provider_id)?;
        saved_with_vault(provider_id, &native_vault()?)
    }

    /// Verifies credentials with the provider and stores only an accepted login in its native vault entry.
    /// Run off the UI thread; submitted fields and their values must not be logged.
    ///
    /// # Errors
    /// Rejects malformed fields, provider refusal or unavailable credential storage.
    pub fn sign_in_provider(
        &self,
        provider_id: &str,
        fields: &[LoginFieldValue<'_>],
    ) -> Result<(), String> {
        Self::credential_provider(provider_id)?;
        let signing_in = &self.runtime.rithmic_login.signing_in;
        signing_in
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "A Rithmic sign-in is already running".to_string())?;
        let stop = Arc::clone(&self.runtime.shutdown);
        let result = native_vault().and_then(|vault| {
            sign_in_with_vault(provider_id, fields, &vault, |credentials| {
                sign_in(credentials, stop)
            })
        });
        signing_in.store(false, Ordering::Release);
        result?;
        self.runtime.rithmic_login.revision.advance();
        Ok(())
    }

    /// Deletes only the selected provider's native vault entry and retires its old session.
    ///
    /// # Errors
    /// Rejects unsupported providers or reports a failed vault deletion.
    pub fn forget_provider_login(&self, provider_id: &str) -> Result<(), String> {
        Self::credential_provider(provider_id)?;
        forget_with_vault(provider_id, &native_vault()?)?;
        self.runtime.rithmic_login.revision.advance();
        Ok(())
    }

    /// Lists the systems one Rithmic server offers, such as
    /// "Rithmic Paper Trading". Discovery needs no credentials. Call from a
    /// background worker: this opens one bounded WSS connection, and only one
    /// discovery runs at a time.
    ///
    /// # Errors
    /// Rejects a concurrent discovery, or reports a redacted connection or
    /// protocol failure.
    fn discover_rithmic_systems(&self, server: RithmicServer) -> Result<Vec<String>, String> {
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
}

impl From<RithmicLoginSummary> for SavedProviderLogin {
    fn from(summary: RithmicLoginSummary) -> Self {
        Self {
            server_id: summary.server.host().to_string(),
            server_label: summary.server.label().to_string(),
            system_name: summary.system_name,
        }
    }
}

fn credentials_from_fields<'a>(
    fields: &'a [LoginFieldValue<'a>],
) -> Result<RithmicCredentials<'a>, String> {
    if fields.len() != LOGIN_SCHEMA.fields.len()
        || fields
            .iter()
            .zip(LOGIN_SCHEMA.fields)
            .any(|(given, required)| given.id != required.id)
    {
        return Err("Invalid provider login fields".to_string());
    }
    let server = RithmicServer::from_id(fields[0].value)
        .ok_or_else(|| "Unknown provider server".to_string())?;
    Ok(RithmicCredentials {
        gateway: server.0,
        system_name: fields[1].value,
        user: fields[2].value,
        password: fields[3].value,
    })
}

fn sign_in_with_vault(
    provider_id: &str,
    fields: &[LoginFieldValue<'_>],
    vault: &impl CredentialVault,
    sign_in: impl FnOnce(RithmicCredentials<'_>) -> Result<(), RithmicSessionError>,
) -> Result<(), String> {
    MarketService::credential_provider(provider_id)?;
    save_signed_in_login(vault, credentials_from_fields(fields)?, sign_in)
}

fn forget_with_vault(provider_id: &str, vault: &impl CredentialVault) -> Result<(), String> {
    MarketService::credential_provider(provider_id)?;
    vault
        .delete(RITHMIC_LOGIN_VAULT_KEY)
        .map_err(|_| "Saved Rithmic login could not be deleted".to_string())
}

fn saved_with_vault(
    provider_id: &str,
    vault: &impl CredentialVault,
) -> Result<Option<SavedProviderLogin>, String> {
    MarketService::credential_provider(provider_id)?;
    load_summary(vault).map(|summary| summary.map(SavedProviderLogin::from))
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

/// Validates the fields, signs in with them, and stores only a login the
/// sign-in accepted.
fn save_signed_in_login(
    vault: &impl CredentialVault,
    credentials: RithmicCredentials<'_>,
    sign_in: impl FnOnce(RithmicCredentials<'_>) -> Result<(), RithmicSessionError>,
) -> Result<(), String> {
    let encoded = RithmicCredentialBytes::try_encode(credentials).map_err(login_field_error)?;
    sign_in(credentials).map_err(|error| sign_in_error(error, credentials))?;
    vault
        .store(RITHMIC_LOGIN_VAULT_KEY, encoded.as_bytes())
        .map_err(|_| "Rithmic login could not be saved to protected storage".to_string())
}

/// Logs in to the ticker plant, the one live market data uses, and logs out.
fn sign_in(
    credentials: RithmicCredentials<'_>,
    stop: Arc<AtomicBool>,
) -> Result<(), RithmicSessionError> {
    let connection = RithmicSession::discover_and_login(
        credentials,
        RithmicApplication {
            name: RITHMIC_APPLICATION_NAME,
            version: env!("CARGO_PKG_VERSION"),
        },
        RithmicSessionLimits::default(),
        Some(stop),
    )?;
    // Rithmic accepted the login; a logout that does not finish cleanly
    // leaves nothing to save differently.
    if let Err(error) = connection.close() {
        diagnostic!("Aeris Rithmic sign-in check did not log out cleanly: {error}");
    }
    Ok(())
}

fn sign_in_error(error: RithmicSessionError, credentials: RithmicCredentials<'_>) -> String {
    let server = RithmicServer(credentials.gateway);
    let (label, host, system) = (server.label(), server.host(), credentials.system_name);
    match error {
        RithmicSessionError::LoginRejected => format!(
            "Rithmic refused this login for {system}. Check the user ID, password and system. \
             If they are right, the account needs a paid market-data subscription and Aeris \
             enabled by its broker or prop firm."
        ),
        RithmicSessionError::SystemUnavailable => {
            format!("The {label} server does not offer {system}. Choose another system.")
        }
        RithmicSessionError::Resolve => {
            format!("Could not find the Rithmic server {host}. Check the internet connection.")
        }
        RithmicSessionError::Connect
        | RithmicSessionError::Tls
        | RithmicSessionError::Handshake => format!(
            "Could not connect to the Rithmic server {host}. Check the internet connection \
             and firewall."
        ),
        RithmicSessionError::Deadline
        | RithmicSessionError::Transport
        | RithmicSessionError::DiscoveryClose => {
            format!("The {label} server stopped answering during sign-in. Try again.")
        }
        RithmicSessionError::Cancelled => "Sign-in stopped because Aeris is closing.".to_string(),
        RithmicSessionError::KitUnavailable
        | RithmicSessionError::InvalidEndpoint
        | RithmicSessionError::InvalidLimits
        | RithmicSessionError::RequestInFlight
        | RithmicSessionError::RequestNotPermitted
        | RithmicSessionError::RequestRejected
        | RithmicSessionError::UnexpectedMessage
        | RithmicSessionError::Protocol
        | RithmicSessionError::DiscoveryRejected
        | RithmicSessionError::SchemaMismatch => format!("Rithmic sign-in failed: {error}."),
    }
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
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryVault(Mutex<BTreeMap<String, Vec<u8>>>);

    impl CredentialVault for MemoryVault {
        type Error = ();
        fn store(&self, key: &str, bytes: &[u8]) -> Result<(), ()> {
            assert_eq!(key, RITHMIC_LOGIN_VAULT_KEY);
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), bytes.to_vec());
            Ok(())
        }
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ()> {
            assert_eq!(key, RITHMIC_LOGIN_VAULT_KEY);
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn delete(&self, key: &str) -> Result<(), ()> {
            assert_eq!(key, RITHMIC_LOGIN_VAULT_KEY);
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[test]
    fn neutral_login_writes_only_the_provider_vault_entry() {
        let vault = MemoryVault::default();
        vault
            .0
            .lock()
            .unwrap()
            .insert("other-provider".into(), b"untouched".to_vec());
        let fields = [
            LoginFieldValue {
                id: "server",
                value: chicago().host(),
            },
            LoginFieldValue {
                id: "system",
                value: "Rithmic Paper Trading",
            },
            LoginFieldValue {
                id: "user",
                value: "fixture-user",
            },
            LoginFieldValue {
                id: "password",
                value: "fixture-password",
            },
        ];
        assert_eq!(
            sign_in_with_vault("hyperliquid", &fields, &vault, |_| panic!(
                "public provider cannot sign in"
            )),
            Err("This provider does not use saved credentials".to_string())
        );
        assert_eq!(vault.0.lock().unwrap().len(), 1);
        assert_eq!(
            saved_with_vault("hyperliquid", &vault),
            Err("This provider does not use saved credentials".to_string())
        );
        assert_eq!(
            forget_with_vault("hyperliquid", &vault),
            Err("This provider does not use saved credentials".to_string())
        );
        sign_in_with_vault("rithmic", &fields, &vault, |_| Ok(())).unwrap();
        assert_eq!(vault.0.lock().unwrap().len(), 2);
        assert_eq!(
            vault.0.lock().unwrap().get("other-provider").unwrap(),
            b"untouched"
        );
        let summary = saved_with_vault("rithmic", &vault).unwrap().unwrap();
        assert_eq!(summary.server_id, chicago().host());
        assert_eq!(summary.server_label, chicago().label());
        assert_eq!(summary.system_name, "Rithmic Paper Trading");
        let summary_text = format!("{summary:?}");
        assert!(!summary_text.contains("fixture-user"));
        assert!(!summary_text.contains("fixture-password"));
        forget_with_vault("rithmic", &vault).unwrap();
        assert_eq!(vault.0.lock().unwrap().len(), 1);
        assert_eq!(
            vault.0.lock().unwrap().get("other-provider").unwrap(),
            b"untouched"
        );
        assert_eq!(saved_with_vault("rithmic", &vault), Ok(None));
    }

    fn chicago() -> RithmicServer {
        RithmicServer::all()
            .find(|server| server.host() == "rprotocol.rithmic.com")
            .expect("Chicago server is listed")
    }

    fn login<'a>(system_name: &'a str, user: &'a str, password: &'a str) -> RithmicCredentials<'a> {
        RithmicCredentials {
            gateway: chicago().0,
            system_name,
            user,
            password,
        }
    }

    #[test]
    fn saved_login_summary_names_server_and_system_but_never_credentials() {
        let vault = MemoryVault::default();
        assert_eq!(load_summary(&vault), Ok(None));
        save_signed_in_login(
            &vault,
            login("Rithmic Paper Trading", "fixture-user", "fixture-password"),
            |_| Ok(()),
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
    fn invalid_login_fields_are_rejected_before_signing_in_or_storage() {
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
                save_signed_in_login(&vault, login(system, user, password), |_| {
                    panic!("invalid fields never reach Rithmic")
                }),
                Err(expected.to_string())
            );
        }
        assert_eq!(load_summary(&vault), Ok(None));
    }

    #[test]
    fn a_login_rithmic_refuses_is_never_saved_and_keeps_the_earlier_one() {
        let vault = MemoryVault::default();
        save_signed_in_login(&vault, login("Rithmic 01", "kept", "kept"), |_| Ok(()))
            .expect("earlier login saves");
        let earlier = vault.load(RITHMIC_LOGIN_VAULT_KEY).unwrap();
        for (refusal, expected) in [
            (
                RithmicSessionError::LoginRejected,
                "Rithmic refused this login for Rithmic Paper Trading. Check the user ID, \
                 password and system. If they are right, the account needs a paid market-data \
                 subscription and Aeris enabled by its broker or prop firm.",
            ),
            (
                RithmicSessionError::SystemUnavailable,
                "The Chicago (core) server does not offer Rithmic Paper Trading. Choose another \
                 system.",
            ),
            (
                RithmicSessionError::Connect,
                "Could not connect to the Rithmic server rprotocol.rithmic.com. Check the \
                 internet connection and firewall.",
            ),
            (
                RithmicSessionError::Deadline,
                "The Chicago (core) server stopped answering during sign-in. Try again.",
            ),
        ] {
            assert_eq!(
                save_signed_in_login(
                    &vault,
                    login("Rithmic Paper Trading", "fixture-user", "fixture-password"),
                    |_| Err(refusal),
                ),
                Err(expected.to_string())
            );
            assert_eq!(vault.load(RITHMIC_LOGIN_VAULT_KEY).unwrap(), earlier);
        }
    }

    #[test]
    fn sign_in_failures_never_echo_credentials() {
        let credentials = login("Rithmic 01", "fixture-user", "fixture-password");
        for error in [
            RithmicSessionError::LoginRejected,
            RithmicSessionError::Resolve,
            RithmicSessionError::Protocol,
        ] {
            let message = sign_in_error(error, credentials);
            assert!(!message.contains("fixture-user"), "{message}");
            assert!(!message.contains("fixture-password"), "{message}");
        }
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
