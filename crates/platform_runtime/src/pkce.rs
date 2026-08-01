//! RFC 7636 PKCE secret generation for native authorization-code flows.
//!
//! Verifiers and CSRF state come from the operating-system CSPRNG. The verifier is
//! never logged, never derived from a clock, and never leaves this process except as
//! the token-exchange parameter it was generated for.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use core::fmt;
use sha2::{Digest, Sha256};
use std::error::Error;

/// Verifier entropy in bytes. 32 bytes encode to a 43-character verifier, the
/// shortest length RFC 7636 section 4.1 permits.
const VERIFIER_ENTROPY_BYTES: usize = 32;
/// CSRF `state` entropy in bytes.
const STATE_ENTROPY_BYTES: usize = 32;

/// The only challenge method Axiusflow accepts. Plain is rejected by construction.
pub const CODE_CHALLENGE_METHOD: &str = "S256";

/// Reason a PKCE secret could not be produced.
#[derive(Debug)]
pub enum PkceError {
    Entropy(getrandom::Error),
}

impl fmt::Display for PkceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entropy(error) => write!(formatter, "PKCE entropy unavailable: {error}"),
        }
    }
}

impl Error for PkceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Entropy(error) => Some(error),
        }
    }
}

/// One single-use PKCE verifier plus its derived challenge and CSRF state.
///
/// `Debug` deliberately redacts both the verifier and the CSRF state so neither can reach
/// logs. A leaked state lets a local caller forge a state-verified redirect, so it is as
/// sensitive as the verifier while authorization is pending.
#[derive(Clone)]
pub struct PkceSecret {
    verifier: String,
    challenge: String,
    state: String,
}

impl PkceSecret {
    /// Generates a verifier, its `S256` challenge, and an independent CSRF state.
    ///
    /// # Errors
    ///
    /// Returns [`PkceError::Entropy`] when the operating-system CSPRNG is unavailable.
    pub fn generate() -> Result<Self, PkceError> {
        let mut verifier_entropy = [0_u8; VERIFIER_ENTROPY_BYTES];
        getrandom::fill(&mut verifier_entropy).map_err(PkceError::Entropy)?;
        let mut state_entropy = [0_u8; STATE_ENTROPY_BYTES];
        getrandom::fill(&mut state_entropy).map_err(PkceError::Entropy)?;

        let verifier = URL_SAFE_NO_PAD.encode(verifier_entropy);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Ok(Self {
            verifier,
            challenge,
            state: URL_SAFE_NO_PAD.encode(state_entropy),
        })
    }

    /// Returns the verifier sent only to the token endpoint.
    #[must_use]
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// Returns the `S256` challenge sent to the authorization endpoint.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// Returns the CSRF state the callback must echo back.
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    /// Returns the challenge method, which is always `S256`.
    #[must_use]
    pub const fn challenge_method(&self) -> &'static str {
        CODE_CHALLENGE_METHOD
    }
}

impl fmt::Debug for PkceSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PkceSecret")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .field("challenge_method", &CODE_CHALLENGE_METHOD)
            .field("state", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{CODE_CHALLENGE_METHOD, PkceSecret, VERIFIER_ENTROPY_BYTES};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    #[test]
    fn verifier_satisfies_rfc_7636_length_and_character_set() {
        let secret = PkceSecret::generate().expect("the operating-system CSPRNG is available");
        let verifier = secret.verifier();
        assert!(
            (43..=128).contains(&verifier.len()),
            "verifier length {} is outside the RFC 7636 range",
            verifier.len()
        );
        assert!(
            verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'.' | b'_' | b'~')),
            "verifier must only use RFC 7636 unreserved characters"
        );
    }

    #[test]
    fn challenge_is_the_s256_digest_of_the_verifier() {
        let secret = PkceSecret::generate().expect("the operating-system CSPRNG is available");
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(secret.verifier().as_bytes()));
        assert_eq!(secret.challenge(), expected);
        assert_eq!(secret.challenge_method(), CODE_CHALLENGE_METHOD);
        assert_ne!(secret.challenge(), secret.verifier());
    }

    #[test]
    fn every_secret_uses_independent_entropy() {
        let first = PkceSecret::generate().expect("the operating-system CSPRNG is available");
        let second = PkceSecret::generate().expect("the operating-system CSPRNG is available");
        assert_ne!(first.verifier(), second.verifier());
        assert_ne!(first.state(), second.state());
        assert_ne!(first.state(), first.verifier());
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(first.verifier())
                .expect("verifier decodes")
                .len(),
            VERIFIER_ENTROPY_BYTES
        );
    }

    #[test]
    fn debug_output_never_reveals_the_verifier_or_state() {
        let secret = PkceSecret::generate().expect("the operating-system CSPRNG is available");
        let rendered = format!("{secret:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(
            !rendered.contains(secret.verifier()),
            "the verifier must never appear in debug output"
        );
        assert!(
            !rendered.contains(secret.state()),
            "the CSRF state must never appear in debug output"
        );
        assert!(
            rendered.contains(secret.challenge()),
            "the public challenge remains observable for diagnostics"
        );
    }
}
