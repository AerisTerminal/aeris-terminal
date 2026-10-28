//! The release configuration packaging embeds at build time.
//!
//! `build.rs` reads every packaging variable once, validates the complete set
//! with `release_config_rules.rs` and generates the static included below. A
//! development build has no release configuration; a partially or invalidly
//! configured release build fails to compile.

/// One release build's packaging configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseConfig {
    /// Unpadded base64url-decoded Ed25519 key that verifies release manifests
    /// and block plans.
    pub verifying_key: [u8; 32],
    /// HTTPS prefix of release channels and block plans, without a trailing slash.
    pub base_url: &'static str,
    /// Lowest install generation a bootstrap install or update accepts.
    pub bootstrap_minimum_generation: u64,
    pub release_identity: &'static str,
    pub install_generation: u64,
    /// SHA-1 thumbprint of the certificate that signs every installed executable.
    #[cfg(target_os = "windows")]
    pub authenticode_thumbprint: [u8; 20],
}

include!(concat!(env!("OUT_DIR"), "/release_config.rs"));

/// Returns the release configuration compiled into this binary, or `None`
/// for a development build.
#[must_use]
pub fn release_config() -> Option<&'static ReleaseConfig> {
    EMBEDDED_RELEASE_CONFIG.as_ref()
}

#[cfg(test)]
#[path = "../release_config_rules.rs"]
mod rules;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::rules::{RELEASE_CONFIG_VARIABLES, validate_release_config};

    const KEY: &str = "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo";
    const THUMBPRINT: &str = "0123456789abcdef0123456789ABCDEF01234567";

    fn complete() -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            ("AERIS_RELEASE_VERIFYING_KEY", KEY.to_string()),
            (
                "AERIS_RELEASE_BASE_URL",
                "https://downloads.example.test/aeris".to_string(),
            ),
            ("AERIS_BOOTSTRAP_MIN_GENERATION", "3".to_string()),
            (
                "AERIS_RELEASE_IDENTITY",
                "release_identity-0001".to_string(),
            ),
            ("AERIS_INSTALL_GENERATION", "7".to_string()),
            ("AERIS_AUTHENTICODE_CERT_SHA1", THUMBPRINT.to_string()),
        ])
    }

    fn validate(
        variables: &BTreeMap<&'static str, String>,
        windows_target: bool,
    ) -> Result<Option<super::rules::ValidatedReleaseConfig>, String> {
        validate_release_config(|name| variables.get(name).cloned(), windows_target)
    }

    #[test]
    fn no_release_variable_is_a_development_build() {
        assert_eq!(validate(&BTreeMap::new(), true), Ok(None));
    }

    #[test]
    fn complete_windows_config_is_embedded_exactly() {
        let config = validate(&complete(), true)
            .expect("valid configuration")
            .expect("release build");
        assert_eq!(config.base_url, "https://downloads.example.test/aeris");
        assert_eq!(config.bootstrap_minimum_generation, 3);
        assert_eq!(config.release_identity, "release_identity-0001");
        assert_eq!(config.install_generation, 7);
        assert_eq!(
            config.authenticode_thumbprint,
            Some([
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
            ])
        );
        assert!(ed25519_dalek::VerifyingKey::from_bytes(&config.verifying_key).is_ok());
    }

    #[test]
    fn any_missing_variable_fails_a_partial_release_build() {
        for missing in RELEASE_CONFIG_VARIABLES {
            let mut variables = complete();
            variables.remove(missing);
            let error = validate(&variables, true).expect_err("partial configuration");
            assert!(error.contains(missing), "{missing}: {error}");
        }
    }

    #[test]
    fn thumbprint_is_windows_only() {
        let mut variables = complete();
        assert!(validate(&variables, false).is_err());
        variables.remove("AERIS_AUTHENTICODE_CERT_SHA1");
        let config = validate(&variables, false)
            .expect("valid configuration")
            .expect("release build");
        assert_eq!(config.authenticode_thumbprint, None);
    }

    #[test]
    fn invalid_values_fail_the_build() {
        for (name, value) in [
            ("AERIS_RELEASE_VERIFYING_KEY", "AAAA"),
            (
                "AERIS_RELEASE_VERIFYING_KEY",
                "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo=",
            ),
            ("AERIS_RELEASE_BASE_URL", "http://downloads.example.test"),
            ("AERIS_RELEASE_BASE_URL", "https://downloads.example.test/"),
            (
                "AERIS_RELEASE_BASE_URL",
                "https://downloads.example.test/?channel",
            ),
            ("AERIS_RELEASE_BASE_URL", "https:///aeris"),
            (
                "AERIS_RELEASE_BASE_URL",
                "https://downloads.example.test/a b",
            ),
            ("AERIS_BOOTSTRAP_MIN_GENERATION", "0"),
            ("AERIS_BOOTSTRAP_MIN_GENERATION", "-1"),
            ("AERIS_RELEASE_IDENTITY", "short"),
            ("AERIS_RELEASE_IDENTITY", "release.identity.0001"),
            ("AERIS_INSTALL_GENERATION", "seven"),
            ("AERIS_AUTHENTICODE_CERT_SHA1", "0123"),
            (
                "AERIS_AUTHENTICODE_CERT_SHA1",
                "g123456789abcdef0123456789abcdef01234567",
            ),
        ] {
            let mut variables = complete();
            variables.insert(name, value.to_string());
            let error = validate(&variables, true).expect_err(value);
            assert!(error.contains(name), "{name}={value}: {error}");
        }
    }

    #[test]
    fn this_build_embeds_a_usable_config_or_none() {
        if let Some(config) = super::release_config() {
            assert!(ed25519_dalek::VerifyingKey::from_bytes(&config.verifying_key).is_ok());
            assert!(config.install_generation > 0);
        }
    }
}
