//! Provider-neutral authentication adapter contract and registry.
//!
//! Credentialed providers implement [`ProviderAuthAdapter`] and register it in
//! [`ProviderAuthRegistry`] so [`MarketService`] dispatches server discovery,
//! sign-in, saved-login lookup, and logout without branching on concrete
//! provider identities.

use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};

use super::{
    MarketService,
    descriptors::{LoginFieldValue, ProviderLoginServer, SavedProviderLogin},
    error::ProviderAuthError,
};

/// Provider-owned authentication and credential lifecycle seam.
pub(super) trait ProviderAuthAdapter: Send + Sync {
    /// Non-secret connection servers published by the provider.
    fn servers(&self) -> Vec<ProviderLoginServer>;

    /// Discovers selectable systems or environments at one server without credentials.
    fn discover_servers(
        &self,
        server_id: &str,
        stop: Arc<AtomicBool>,
    ) -> Result<Vec<String>, ProviderAuthError>;

    /// Loads non-secret metadata for the saved login, if any.
    fn saved_login(&self) -> Result<Option<SavedProviderLogin>, ProviderAuthError>;

    /// Verifies submitted credentials with the provider and persists only an accepted login.
    fn sign_in(
        &self,
        fields: &[LoginFieldValue<'_>],
        stop: Arc<AtomicBool>,
    ) -> Result<(), ProviderAuthError>;

    /// Deletes the saved login and notifies active workers so retired sessions close.
    fn forget_login(&self) -> Result<(), ProviderAuthError>;
}

/// Runtime-owned map of credentialed providers keyed by provider ID.
#[derive(Default)]
pub(super) struct ProviderAuthRegistry {
    adapters: BTreeMap<&'static str, Arc<dyn ProviderAuthAdapter>>,
}

impl ProviderAuthRegistry {
    pub(super) fn register(
        &mut self,
        provider_id: &'static str,
        adapter: Arc<dyn ProviderAuthAdapter>,
    ) {
        self.adapters.insert(provider_id, adapter);
    }

    fn adapter(&self, provider_id: &str) -> Result<&dyn ProviderAuthAdapter, ProviderAuthError> {
        if let Some(adapter) = self.adapters.get(provider_id) {
            return Ok(adapter.as_ref());
        }
        let registration = super::BUILT_IN_PROVIDER_DESCRIPTORS
            .iter()
            .find(|provider| provider.id == provider_id)
            .ok_or(ProviderAuthError::UnsupportedProvider)?;
        if registration.login_schema.is_none() {
            return Err(ProviderAuthError::NoSavedCredentials);
        }
        Err(ProviderAuthError::Unavailable(
            "Provider login is unavailable",
        ))
    }
}

impl MarketService {
    /// The non-secret servers offered by a registered vault login provider.
    ///
    /// # Errors
    /// Rejects unknown or public providers.
    pub fn provider_login_servers(
        &self,
        provider_id: &str,
    ) -> Result<Vec<ProviderLoginServer>, String> {
        let adapter = self.runtime.provider_auth.adapter(provider_id)?;
        Ok(adapter.servers())
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
        let adapter = self.runtime.provider_auth.adapter(provider_id)?;
        adapter
            .discover_servers(server_id, Arc::clone(&self.runtime.shutdown))
            .map_err(String::from)
    }

    /// Reads only non-secret saved-login metadata from the provider's native vault entry.
    ///
    /// # Errors
    /// Rejects unsupported providers or unreadable vault entries.
    pub fn saved_provider_login(
        &self,
        provider_id: &str,
    ) -> Result<Option<SavedProviderLogin>, String> {
        let adapter = self.runtime.provider_auth.adapter(provider_id)?;
        adapter.saved_login().map_err(String::from)
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
        let adapter = self.runtime.provider_auth.adapter(provider_id)?;
        adapter
            .sign_in(fields, Arc::clone(&self.runtime.shutdown))
            .map_err(String::from)
    }

    /// Deletes only the selected provider's native vault entry and retires its old session.
    ///
    /// # Errors
    /// Rejects unsupported providers or reports a failed vault deletion.
    pub fn forget_provider_login(&self, provider_id: &str) -> Result<(), String> {
        let adapter = self.runtime.provider_auth.adapter(provider_id)?;
        adapter.forget_login().map_err(String::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyAuth;

    impl ProviderAuthAdapter for DummyAuth {
        fn servers(&self) -> Vec<ProviderLoginServer> {
            vec![ProviderLoginServer {
                id: "mock.example",
                label: "Mock",
            }]
        }

        fn discover_servers(
            &self,
            _server_id: &str,
            _stop: Arc<AtomicBool>,
        ) -> Result<Vec<String>, ProviderAuthError> {
            Ok(vec!["Mock System".to_string()])
        }

        fn saved_login(&self) -> Result<Option<SavedProviderLogin>, ProviderAuthError> {
            Ok(None)
        }

        fn sign_in(
            &self,
            _fields: &[LoginFieldValue<'_>],
            _stop: Arc<AtomicBool>,
        ) -> Result<(), ProviderAuthError> {
            Ok(())
        }

        fn forget_login(&self) -> Result<(), ProviderAuthError> {
            Ok(())
        }
    }

    #[test]
    fn registry_rejects_public_and_unknown_providers_and_dispatches_registered_adapters() {
        let mut registry = ProviderAuthRegistry::default();
        assert_eq!(
            registry.adapter("hyperliquid").err(),
            Some(ProviderAuthError::NoSavedCredentials)
        );
        assert_eq!(
            registry.adapter("unknown-provider").err(),
            Some(ProviderAuthError::UnsupportedProvider)
        );
        assert_eq!(
            registry.adapter("rithmic").err(),
            Some(ProviderAuthError::Unavailable(
                "Provider login is unavailable"
            ))
        );
        registry.register("mock-broker", Arc::new(DummyAuth));
        let adapter = registry
            .adapter("mock-broker")
            .expect("registered auth adapter resolves");
        assert_eq!(adapter.servers().len(), 1);
        assert_eq!(
            adapter.discover_servers("mock.example", Arc::new(AtomicBool::new(false))),
            Ok(vec!["Mock System".to_string()])
        );
    }
}
