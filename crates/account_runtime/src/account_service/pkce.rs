//! PKCE (RFC 7636) and OAuth state/nonce generation for native login.
//!
//! The verifier is 32 random bytes; the challenge is the base64url-encoded
//! SHA-256 of the verifier with the `S256` method. State and nonce are
//! 16 random bytes each, base64url-encoded. All material is generated with
//! the OS CSPRNG and the verifier is zeroized on drop.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

/// PKCE code verifier: 32 random bytes, base64url-encoded (43 characters).
#[derive(Debug)]
pub struct PkceVerifier {
    encoded: Zeroizing<String>,
}

impl PkceVerifier {
    /// Generates one verifier from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// Returns an error when the system CSPRNG is unavailable.
    pub fn generate() -> Result<Self, String> {
        let mut raw = Zeroizing::new(vec![0_u8; 32]);
        getrandom::fill(raw.as_mut_slice()).map_err(|_| "system CSPRNG unavailable".to_string())?;
        Ok(Self::from_bytes(&raw))
    }

    fn from_bytes(raw: &[u8]) -> Self {
        Self {
            encoded: Zeroizing::new(URL_SAFE_NO_PAD.encode(raw)),
        }
    }

    /// Returns the encoded verifier sent to the token endpoint.
    #[must_use]
    pub fn encoded(&self) -> &str {
        self.encoded.as_str()
    }

    /// Returns the `S256` code challenge for the authorization URL.
    #[must_use]
    pub fn challenge(&self) -> String {
        challenge_for_encoded(self.encoded.as_str())
    }
}

/// Returns the RFC 7636 `S256` challenge for one encoded verifier.
#[must_use]
pub fn challenge_for_encoded(encoded_verifier: &str) -> String {
    let digest = Sha256::digest(encoded_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generates one base64url OAuth state or nonce value (16 random bytes).
///
/// # Errors
///
/// Returns an error when the system CSPRNG is unavailable.
pub fn generate_oauth_random() -> Result<String, String> {
    let mut raw = [0_u8; 16];
    getrandom::fill(&mut raw).map_err(|_| "system CSPRNG unavailable".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

#[cfg(test)]
mod tests {
    use super::{PkceVerifier, challenge_for_encoded, generate_oauth_random};

    #[test]
    fn rfc7636_challenge_vector_matches() {
        // RFC 7636 appendix B: verifier `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk`
        // challenges to `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM`.
        assert_eq!(
            challenge_for_encoded("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_s256_sized_and_unique() {
        let first = PkceVerifier::generate().expect("verifier generates");
        let second = PkceVerifier::generate().expect("verifier generates");
        assert_eq!(first.encoded().len(), 43);
        assert_ne!(first.encoded(), second.encoded());
        assert_eq!(first.challenge().len(), 43);
    }

    #[test]
    fn oauth_random_values_are_unique() {
        let first = generate_oauth_random().expect("random generates");
        let second = generate_oauth_random().expect("random generates");
        assert_eq!(first.len(), 22);
        assert_ne!(first, second);
    }
}
