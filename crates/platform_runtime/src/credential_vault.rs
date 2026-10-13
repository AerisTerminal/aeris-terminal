//! Protected credential storage boundary and native operating-system adapter.

use std::{
    error::Error,
    fmt,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use keyring::{Entry, Error as KeyringError};

static NATIVE_CREDENTIAL_ACCESS: Mutex<()> = Mutex::new(());
static BORROWED_CTRADER_LOGGED: AtomicBool = AtomicBool::new(false);
const BORROWED_CTRADER_NOTICE: &str = "vault: borrowing stored cTrader connection";

fn log_borrowed_ctrader_once(logged: &AtomicBool, emit: impl FnOnce(&str)) {
    if !logged.swap(true, Ordering::AcqRel) {
        emit(BORROWED_CTRADER_NOTICE);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
trait NativeCredentialBackend: fmt::Debug + Send + Sync {
    fn store(&self, service: &str, key: &str, secret: &[u8]) -> Result<(), KeyringError>;
    fn load(&self, service: &str, key: &str) -> Result<Vec<u8>, KeyringError>;
    fn delete(&self, service: &str, key: &str) -> Result<(), KeyringError>;
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug)]
struct KeyringCredentialBackend;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl NativeCredentialBackend for KeyringCredentialBackend {
    fn store(&self, service: &str, key: &str, secret: &[u8]) -> Result<(), KeyringError> {
        Entry::new(service, key)?.set_secret(secret)
    }

    fn load(&self, service: &str, key: &str) -> Result<Vec<u8>, KeyringError> {
        Entry::new(service, key)?.get_secret()
    }

    fn delete(&self, service: &str, key: &str) -> Result<(), KeyringError> {
        Entry::new(service, key)?.delete_credential()
    }
}

/// Secure credential storage boundary. Implementations must never use plaintext files.
pub trait CredentialVault {
    type Error;

    /// Stores an opaque secret under a non-secret key.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage is unavailable.
    fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error>;

    /// Loads an opaque secret, if present.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage cannot be queried.
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Deletes an opaque secret.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage cannot be updated.
    fn delete(&self, key: &str) -> Result<(), Self::Error>;
}

/// Errors returned by the native operating-system credential vault.
#[derive(Debug)]
pub enum NativeCredentialVaultError {
    InvalidService,
    InvalidKey,
    AccessSerialized,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    Platform(KeyringError),
    UnsupportedPlatform,
}

impl fmt::Display for NativeCredentialVaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidService => formatter.write_str("credential vault service is invalid"),
            Self::InvalidKey => formatter.write_str("credential vault key is invalid"),
            Self::AccessSerialized => {
                formatter.write_str("credential vault access lock is unavailable")
            }
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            Self::Platform(error) => {
                write!(formatter, "operating-system credential vault: {error}")
            }
            Self::UnsupportedPlatform => {
                formatter.write_str("no native credential vault exists for this platform")
            }
        }
    }
}

impl Error for NativeCredentialVaultError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            Self::Platform(error) => Some(error),
            _ => None,
        }
    }
}

/// Credential vault backed by Linux Secret Service, macOS Keychain, or Windows
/// Credential Manager.
pub struct NativeCredentialVault {
    service: String,
    routing: VaultRouting,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    backend: Box<dyn NativeCredentialBackend>,
}

#[derive(Clone)]
enum VaultRouting {
    Production,
    Validation {
        namespace: String,
        borrow_stored_ctrader: bool,
    },
}

impl fmt::Debug for NativeCredentialVault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCredentialVault")
            .field("service", &self.service)
            .field(
                "namespace",
                &match &self.routing {
                    VaultRouting::Production => "production",
                    VaultRouting::Validation { namespace, .. } => namespace,
                },
            )
            .finish_non_exhaustive()
    }
}

impl NativeCredentialVault {
    /// Creates a vault namespace for one application service.
    ///
    /// # Errors
    ///
    /// Returns an error when `service` is empty or contains control characters.
    pub fn new(service: impl Into<String>) -> Result<Self, NativeCredentialVaultError> {
        let service = service.into();
        validate_identifier(&service).map_err(|()| NativeCredentialVaultError::InvalidService)?;
        let routing =
            crate::data_root::active_data_root().map_or(VaultRouting::Production, |root| {
                if root.is_validation() {
                    VaultRouting::Validation {
                        namespace: root.vault_namespace().to_string(),
                        borrow_stored_ctrader: root.borrow_stored_ctrader(),
                    }
                } else {
                    VaultRouting::Production
                }
            });
        Ok(Self {
            service,
            routing,
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            backend: Box::new(KeyringCredentialBackend),
        })
    }

    #[cfg(all(
        test,
        any(target_os = "linux", target_os = "macos", target_os = "windows")
    ))]
    fn with_backend(
        service: impl Into<String>,
        backend: impl NativeCredentialBackend + 'static,
    ) -> Result<Self, NativeCredentialVaultError> {
        let service = service.into();
        validate_identifier(&service).map_err(|()| NativeCredentialVaultError::InvalidService)?;
        Ok(Self {
            service,
            routing: VaultRouting::Production,
            backend: Box::new(backend),
        })
    }

    #[cfg(all(
        test,
        any(target_os = "linux", target_os = "macos", target_os = "windows")
    ))]
    fn with_validation_backend(
        service: impl Into<String>,
        namespace: &str,
        borrow_stored_ctrader: bool,
        backend: impl NativeCredentialBackend + 'static,
    ) -> Result<Self, NativeCredentialVaultError> {
        let service = service.into();
        validate_identifier(&service).map_err(|()| NativeCredentialVaultError::InvalidService)?;
        Ok(Self {
            service,
            routing: VaultRouting::Validation {
                namespace: namespace.to_string(),
                borrow_stored_ctrader,
            },
            backend: Box::new(backend),
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn with_backend_access<T>(
        &self,
        key: &str,
        operation: impl FnOnce(
            &dyn NativeCredentialBackend,
            &str,
            &str,
            &[(String, String)],
        ) -> Result<T, NativeCredentialVaultError>,
    ) -> Result<T, NativeCredentialVaultError> {
        validate_identifier(key).map_err(|()| NativeCredentialVaultError::InvalidKey)?;
        let _guard = NATIVE_CREDENTIAL_ACCESS
            .lock()
            .map_err(|_| NativeCredentialVaultError::AccessSerialized)?;
        let production_access = match &self.routing {
            VaultRouting::Production => true,
            VaultRouting::Validation {
                borrow_stored_ctrader,
                ..
            } => {
                *borrow_stored_ctrader
                    && self.service == "aeris.provider.ctrader"
                    && key == "broker_connection"
            }
        };
        let namespace = match &self.routing {
            VaultRouting::Validation { namespace, .. } if !production_access => namespace.clone(),
            VaultRouting::Production | VaultRouting::Validation { .. } => "aeris".to_string(),
        };
        let service = previous_backend_identifier(&namespace, "service", &self.service);
        let canonical_key = previous_backend_identifier(&namespace, "key", key);
        let legacy = if production_access {
            legacy_services(&self.service)
                .iter()
                .map(|(namespace, service)| {
                    (
                        previous_backend_identifier(namespace, "service", service),
                        previous_backend_identifier(namespace, "key", key),
                    )
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        operation(self.backend.as_ref(), &service, &canonical_key, &legacy)
    }
}

pub(crate) fn log_validation_borrow_once() {
    log_borrowed_ctrader_once(&BORROWED_CTRADER_LOGGED, |notice| {
        aeris_observability::diagnostic!("{notice}");
    });
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl CredentialVault for NativeCredentialVault {
    type Error = NativeCredentialVaultError;

    fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error> {
        self.with_backend_access(key, |backend, service, key, legacy| {
            backend
                .store(service, key, secret)
                .map_err(NativeCredentialVaultError::Platform)?;
            for (legacy_service, legacy_key) in legacy {
                match backend.delete(legacy_service, legacy_key) {
                    Ok(()) | Err(KeyringError::NoEntry) => {}
                    Err(error) => return Err(NativeCredentialVaultError::Platform(error)),
                }
            }
            Ok(())
        })
    }

    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        self.with_backend_access(key, |backend, service, key, legacy| {
            match backend.load(service, key) {
                Ok(secret) => return Ok(Some(secret)),
                Err(KeyringError::NoEntry) => {}
                Err(error) => return Err(NativeCredentialVaultError::Platform(error)),
            }
            for (legacy_service, legacy_key) in legacy {
                let secret = match backend.load(legacy_service, legacy_key) {
                    Ok(secret) => secret,
                    Err(KeyringError::NoEntry) => continue,
                    Err(error) => return Err(NativeCredentialVaultError::Platform(error)),
                };
                backend
                    .store(service, key, &secret)
                    .map_err(NativeCredentialVaultError::Platform)?;
                for (old_service, old_key) in legacy {
                    match backend.delete(old_service, old_key) {
                        Ok(()) | Err(KeyringError::NoEntry) => {}
                        Err(error) => return Err(NativeCredentialVaultError::Platform(error)),
                    }
                }
                return Ok(Some(secret));
            }
            Ok(None)
        })
    }

    fn delete(&self, key: &str) -> Result<(), Self::Error> {
        self.with_backend_access(key, |backend, service, key, legacy| {
            match backend.delete(service, key) {
                Ok(()) | Err(KeyringError::NoEntry) => {}
                Err(error) => return Err(NativeCredentialVaultError::Platform(error)),
            }
            for (legacy_service, legacy_key) in legacy {
                match backend.delete(legacy_service, legacy_key) {
                    Ok(()) | Err(KeyringError::NoEntry) => {}
                    Err(error) => return Err(NativeCredentialVaultError::Platform(error)),
                }
            }
            Ok(())
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
impl CredentialVault for NativeCredentialVault {
    type Error = NativeCredentialVaultError;

    fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
        Err(NativeCredentialVaultError::UnsupportedPlatform)
    }

    fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        Err(NativeCredentialVaultError::UnsupportedPlatform)
    }

    fn delete(&self, _key: &str) -> Result<(), Self::Error> {
        Err(NativeCredentialVaultError::UnsupportedPlatform)
    }
}

fn validate_identifier(identifier: &str) -> Result<(), ()> {
    if identifier.is_empty() || identifier.chars().any(char::is_control) {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
fn backend_identifier(kind: &str, identifier: &str) -> String {
    previous_backend_identifier("aeris", kind, identifier)
}

fn previous_backend_identifier(namespace: &str, kind: &str, identifier: &str) -> String {
    format!(
        "{namespace}-v1-{kind}-{}",
        URL_SAFE_NO_PAD.encode(identifier)
    )
}

#[cfg(test)]
fn legacy_backend_identifier(kind: &str, identifier: &str) -> String {
    previous_backend_identifier("axiusflow", kind, identifier)
}

fn legacy_services(service: &str) -> &'static [(&'static str, &'static str)] {
    match service {
        "com.aeris.account" => &[
            ("tradingplot", "com.tradingplot.account"),
            ("axiusflow", "com.axiusflow.account"),
        ],
        "com.aeris.terminal" => &[
            ("tradingplot", "com.tradingplot.terminal"),
            ("axiusflow", "com.axiusflow.terminal"),
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BORROWED_CTRADER_NOTICE, CredentialVault, KeyringError, NativeCredentialBackend,
        NativeCredentialVault, NativeCredentialVaultError, backend_identifier,
        legacy_backend_identifier, log_borrowed_ctrader_once, validate_identifier,
    };
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex, atomic::AtomicBool},
    };

    type MemorySecrets = Arc<Mutex<BTreeMap<(String, String), Vec<u8>>>>;

    #[derive(Clone, Debug, Default)]
    struct MemoryCredentialBackend {
        secrets: MemorySecrets,
    }

    impl NativeCredentialBackend for MemoryCredentialBackend {
        fn store(&self, service: &str, key: &str, secret: &[u8]) -> Result<(), KeyringError> {
            self.secrets
                .lock()
                .expect("memory backend lock is available")
                .insert((service.to_string(), key.to_string()), secret.to_vec());
            Ok(())
        }

        fn load(&self, service: &str, key: &str) -> Result<Vec<u8>, KeyringError> {
            self.secrets
                .lock()
                .expect("memory backend lock is available")
                .get(&(service.to_string(), key.to_string()))
                .cloned()
                .ok_or(KeyringError::NoEntry)
        }

        fn delete(&self, service: &str, key: &str) -> Result<(), KeyringError> {
            let removed = self
                .secrets
                .lock()
                .expect("memory backend lock is available")
                .remove(&(service.to_string(), key.to_string()));
            removed.map_or(Err(KeyringError::NoEntry), |_| Ok(()))
        }
    }

    #[test]
    fn native_vault_requires_safe_identifiers() {
        assert!(matches!(
            NativeCredentialVault::new(""),
            Err(NativeCredentialVaultError::InvalidService)
        ));
        assert!(matches!(
            NativeCredentialVault::new("aeris\nterminal"),
            Err(NativeCredentialVaultError::InvalidService)
        ));
        assert!(NativeCredentialVault::new("com.aeris.terminal").is_ok());
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("session\0token").is_err());
        assert!(validate_identifier("session-token").is_ok());
    }

    #[test]
    fn native_vault_round_trips_binary_secrets_and_treats_deletion_as_idempotent() {
        let vault = NativeCredentialVault::with_backend(
            "com.aeris.terminal",
            MemoryCredentialBackend::default(),
        )
        .expect("valid service creates a native vault");
        let secret = [0, 255, 17, 0, 42];

        assert_eq!(vault.load("session").expect("missing load succeeds"), None);
        vault
            .store("session", &secret)
            .expect("binary secret is stored");
        assert_eq!(
            vault.load("session").expect("stored secret is loaded"),
            Some(secret.to_vec())
        );
        vault.delete("session").expect("stored secret is deleted");
        vault
            .delete("session")
            .expect("deleting a missing secret is idempotent");
        assert_eq!(vault.load("session").expect("deleted load succeeds"), None);

        let first = NativeCredentialVault::with_backend("b.c", MemoryCredentialBackend::default())
            .expect("first collision regression vault is valid");
        let second = NativeCredentialVault::with_backend("c", MemoryCredentialBackend::default())
            .expect("second collision regression vault is valid");
        assert_ne!(
            backend_identifier("service", &first.service),
            backend_identifier("service", &second.service)
        );
        assert_ne!(
            format!(
                "{}.{}",
                backend_identifier("key", "a"),
                backend_identifier("service", &first.service)
            ),
            format!(
                "{}.{}",
                backend_identifier("key", "a.b"),
                backend_identifier("service", &second.service)
            )
        );
    }

    #[test]
    fn native_vault_debug_never_contains_secret_material() {
        let vault = NativeCredentialVault::new("com.aeris.terminal")
            .expect("valid service creates a native vault");
        let debug = format!("{vault:?}");
        assert!(debug.contains("com.aeris.terminal"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn native_vault_migrates_legacy_service_and_backend_namespace_on_load() {
        let backend = MemoryCredentialBackend::default();
        let vault = NativeCredentialVault::with_backend("com.aeris.account", backend.clone())
            .expect("canonical account service creates a vault");
        let legacy_service = legacy_backend_identifier("service", "com.axiusflow.account");
        let legacy_key = legacy_backend_identifier("key", "session");
        backend
            .store(&legacy_service, &legacy_key, b"legacy-secret")
            .expect("legacy fixture is stored");

        assert_eq!(
            vault.load("session").expect("legacy credential migrates"),
            Some(b"legacy-secret".to_vec())
        );
        let canonical_service = backend_identifier("service", "com.aeris.account");
        let canonical_key = backend_identifier("key", "session");
        assert_eq!(
            backend
                .load(&canonical_service, &canonical_key)
                .expect("canonical credential exists"),
            b"legacy-secret"
        );
        assert!(matches!(
            backend.load(&legacy_service, &legacy_key),
            Err(KeyringError::NoEntry)
        ));
    }

    #[test]
    fn native_vault_migrates_tradingplot_provider_credential() {
        let backend = MemoryCredentialBackend::default();
        let vault = NativeCredentialVault::with_backend("com.aeris.terminal", backend.clone())
            .expect("canonical terminal service creates a vault");
        let legacy_service = super::previous_backend_identifier(
            "tradingplot",
            "service",
            "com.tradingplot.terminal",
        );
        let legacy_key = super::previous_backend_identifier("tradingplot", "key", "provider");
        backend
            .store(&legacy_service, &legacy_key, b"provider-secret")
            .expect("legacy provider credential");

        assert_eq!(
            vault
                .load("provider")
                .expect("provider credential migrates"),
            Some(b"provider-secret".to_vec())
        );
        assert!(matches!(
            backend.load(&legacy_service, &legacy_key),
            Err(KeyringError::NoEntry)
        ));
    }

    #[test]
    fn native_vault_delete_removes_legacy_credential_when_canonical_is_missing() {
        let backend = MemoryCredentialBackend::default();
        let vault = NativeCredentialVault::with_backend("com.aeris.terminal", backend.clone())
            .expect("canonical terminal service creates a vault");
        let legacy_service = legacy_backend_identifier("service", "com.axiusflow.terminal");
        let legacy_key = legacy_backend_identifier("key", "provider");
        backend
            .store(&legacy_service, &legacy_key, b"legacy-secret")
            .expect("legacy fixture is stored");

        vault
            .delete("provider")
            .expect("legacy-only credential deletion succeeds");
        assert!(matches!(
            backend.load(&legacy_service, &legacy_key),
            Err(KeyringError::NoEntry)
        ));
    }

    #[test]
    fn native_vault_store_retires_legacy_credential() {
        let backend = MemoryCredentialBackend::default();
        let vault = NativeCredentialVault::with_backend("com.aeris.account", backend.clone())
            .expect("canonical account service creates a vault");
        let legacy_service = legacy_backend_identifier("service", "com.axiusflow.account");
        let legacy_key = legacy_backend_identifier("key", "session");
        backend
            .store(&legacy_service, &legacy_key, b"old-secret")
            .expect("legacy fixture is stored");

        vault
            .store("session", b"new-secret")
            .expect("canonical credential replaces legacy state");
        assert_eq!(
            vault.load("session").expect("canonical credential loads"),
            Some(b"new-secret".to_vec())
        );
        assert!(matches!(
            backend.load(&legacy_service, &legacy_key),
            Err(KeyringError::NoEntry)
        ));
    }

    #[test]
    fn validation_vault_never_probes_production_without_opt_in() {
        let backend = MemoryCredentialBackend::default();
        let production_service = backend_identifier("service", "aeris.provider.ctrader");
        let production_key = backend_identifier("key", "broker_connection");
        backend
            .store(&production_service, &production_key, b"production")
            .expect("production fixture");
        let isolated = NativeCredentialVault::with_validation_backend(
            "aeris.provider.ctrader",
            "0123456789ab",
            false,
            backend.clone(),
        )
        .expect("validation vault");

        assert_eq!(
            isolated.load("broker_connection").expect("isolated load"),
            None
        );
        isolated
            .store("broker_connection", b"validation")
            .expect("isolated store");
        assert_eq!(
            backend
                .load(&production_service, &production_key)
                .expect("production remains"),
            b"production"
        );
    }

    #[test]
    fn validation_ctrader_opt_in_is_scoped_to_the_exact_connection_key() {
        let backend = MemoryCredentialBackend::default();
        let borrowed = NativeCredentialVault::with_validation_backend(
            "aeris.provider.ctrader",
            "0123456789ab",
            true,
            backend.clone(),
        )
        .expect("validation vault");
        backend
            .store(
                &backend_identifier("service", "aeris.provider.ctrader"),
                &backend_identifier("key", "broker_connection"),
                b"production",
            )
            .expect("production fixture");

        assert_eq!(
            borrowed.load("broker_connection").expect("borrowed load"),
            Some(b"production".to_vec())
        );
        assert_eq!(borrowed.load("other").expect("scoped load"), None);
        let other_service = NativeCredentialVault::with_validation_backend(
            "aeris.provider.tastytrade",
            "0123456789ab",
            true,
            backend,
        )
        .expect("other validation vault");
        assert_eq!(
            other_service
                .load("broker_connection")
                .expect("other service load"),
            None
        );
    }

    #[test]
    fn validation_ctrader_borrow_notice_is_exact_and_emitted_once() {
        let logged = AtomicBool::new(false);
        let notices = Mutex::new(Vec::new());
        for _ in 0..3 {
            log_borrowed_ctrader_once(&logged, |notice| {
                notices.lock().expect("notices").push(notice.to_string());
            });
        }
        assert_eq!(
            notices.into_inner().expect("notices"),
            [BORROWED_CTRADER_NOTICE.to_string()]
        );
    }
}
