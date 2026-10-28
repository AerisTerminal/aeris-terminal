//! Validation rules for the release configuration packaging embeds at build
//! time. `build.rs` applies them before the crate compiles, and the crate's
//! tests include this file to exercise them.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

/// Every packaging variable of one release configuration. Setting any of them
/// makes the build a release build, which then requires the complete set.
pub const RELEASE_CONFIG_VARIABLES: [&str; 6] = [
    "AERIS_RELEASE_VERIFYING_KEY",
    "AERIS_RELEASE_BASE_URL",
    "AERIS_BOOTSTRAP_MIN_GENERATION",
    "AERIS_RELEASE_IDENTITY",
    "AERIS_INSTALL_GENERATION",
    "AERIS_AUTHENTICODE_CERT_SHA1",
];

/// A complete release configuration whose every value passed validation.
#[derive(Debug, Eq, PartialEq)]
pub struct ValidatedReleaseConfig {
    pub verifying_key: [u8; 32],
    pub base_url: String,
    pub bootstrap_minimum_generation: u64,
    pub release_identity: String,
    pub install_generation: u64,
    /// Present exactly for Windows targets.
    pub authenticode_thumbprint: Option<String>,
}

/// Returns `None` when no release variable is set (a development build) and
/// the validated configuration when all are set. A partial or invalid set is
/// an error naming the variable at fault.
pub fn validate_release_config(
    lookup: impl Fn(&str) -> Option<String>,
    windows_target: bool,
) -> Result<Option<ValidatedReleaseConfig>, String> {
    if RELEASE_CONFIG_VARIABLES
        .iter()
        .all(|name| lookup(name).is_none())
    {
        return Ok(None);
    }
    let required = |name: &str| {
        lookup(name).ok_or_else(|| {
            format!("{name} is required because another release configuration variable is set")
        })
    };
    let verifying_key = URL_SAFE_NO_PAD
        .decode(required("AERIS_RELEASE_VERIFYING_KEY")?)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| {
            "AERIS_RELEASE_VERIFYING_KEY must be a 32-byte unpadded base64url Ed25519 public key"
                .to_string()
        })?;
    let base_url = required("AERIS_RELEASE_BASE_URL")?;
    if !valid_release_base_url(&base_url) {
        return Err(
            "AERIS_RELEASE_BASE_URL must be an https:// URL without a trailing slash, query or fragment"
                .to_string(),
        );
    }
    let bootstrap_minimum_generation =
        positive_generation("AERIS_BOOTSTRAP_MIN_GENERATION", &required)?;
    let release_identity = required("AERIS_RELEASE_IDENTITY")?;
    if !valid_release_identity(&release_identity) {
        return Err(
            "AERIS_RELEASE_IDENTITY must be 16 to 128 ASCII letters, digits, '_' or '-'"
                .to_string(),
        );
    }
    let install_generation = positive_generation("AERIS_INSTALL_GENERATION", &required)?;
    let authenticode_thumbprint = if windows_target {
        let thumbprint = required("AERIS_AUTHENTICODE_CERT_SHA1")?;
        if thumbprint.len() != 40 || !thumbprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(
                "AERIS_AUTHENTICODE_CERT_SHA1 must be a 40-digit hexadecimal SHA-1 thumbprint"
                    .to_string(),
            );
        }
        Some(thumbprint)
    } else if lookup("AERIS_AUTHENTICODE_CERT_SHA1").is_some() {
        return Err("AERIS_AUTHENTICODE_CERT_SHA1 applies only to Windows targets".to_string());
    } else {
        None
    };
    Ok(Some(ValidatedReleaseConfig {
        verifying_key,
        base_url,
        bootstrap_minimum_generation,
        release_identity,
        install_generation,
        authenticode_thumbprint,
    }))
}

fn positive_generation(
    name: &str,
    required: &impl Fn(&str) -> Result<String, String>,
) -> Result<u64, String> {
    required(name)?
        .parse::<u64>()
        .ok()
        .filter(|generation| *generation > 0)
        .ok_or_else(|| format!("{name} must be a positive integer"))
}

fn valid_release_base_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && url.bytes().all(|byte| byte.is_ascii_graphic())
        && !url.ends_with('/')
        && !url.contains(['?', '#', '"', '\\'])
}

fn valid_release_identity(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
