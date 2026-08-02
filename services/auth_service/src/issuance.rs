//! Ed25519 token issuance and revisioned JWKS publication.
//!
//! Issued tokens match the `crates/security` verifier contract exactly: `EdDSA`
//! over `{"alg":"EdDSA","typ":"JWT","kid":..}`, claims `iss`, `aud`, `sub`,
//! `exp`, `purpose=service_access`, plus optional `session_id`/`device_id`.
//! The JWKS document carries the current key with its revision so offline
//! verifiers pin an immutable snapshot.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

/// One file-provisioned Ed25519 signing key with its publication revision.
pub struct IssuanceKey {
    signing: SigningKey,
    key_id: String,
    revision: u64,
}

impl IssuanceKey {
    /// Loads a 32-byte raw Ed25519 signing key from a file.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable files or invalid key material.
    pub fn load(path: &str, key_id: &str, revision: u64) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|error| format!("cannot read signing key: {error}"))?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "signing key must be 32 raw bytes".to_string())?;
        if key_id.is_empty() || key_id.len() > 64 || revision == 0 {
            return Err("invalid key id or revision".to_string());
        }
        Ok(Self {
            signing: SigningKey::from_bytes(&key_bytes),
            key_id: key_id.to_string(),
            revision,
        })
    }

    /// The publication revision of this key.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The verifying half.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// The JWKS document `crates/security` accepts at this revision.
    #[must_use]
    pub fn jwks_json(&self) -> String {
        format!(
            "{{\"keys\":[{{\"kty\":\"OKP\",\"crv\":\"Ed25519\",\"alg\":\"EdDSA\",\"use\":\"sig\",\"kid\":\"{}\",\"x\":\"{}\"}}]}}",
            self.key_id,
            URL_SAFE_NO_PAD.encode(self.verifying_key().as_bytes())
        )
    }

    /// Issues one short-lived service-access token.
    ///
    /// # Errors
    ///
    /// Returns an error when claims overflow the verifier's bounds.
    pub fn issue_token(
        &self,
        subject: &str,
        issuer: &str,
        audience: &str,
        expires_at_unix_seconds: u64,
        session_id: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<String, String> {
        for (name, value) in [
            ("subject", subject),
            ("issuer", issuer),
            ("audience", audience),
        ] {
            if value.is_empty() || value.len() > 256 {
                return Err(format!("{name} outside verifier bounds"));
            }
        }
        let header = URL_SAFE_NO_PAD.encode(format!(
            "{{\"alg\":\"EdDSA\",\"typ\":\"JWT\",\"kid\":\"{}\"}}",
            self.key_id
        ));
        let mut claims = format!(
            "{{\"iss\":\"{issuer}\",\"aud\":\"{audience}\",\"sub\":\"{subject}\",\"exp\":{expires_at_unix_seconds},\"purpose\":\"service_access\""
        );
        if let Some(session_id) = session_id {
            let _ = std::fmt::Write::write_fmt(
                &mut claims,
                format_args!(",\"session_id\":\"{session_id}\""),
            );
        }
        if let Some(device_id) = device_id {
            let _ = std::fmt::Write::write_fmt(
                &mut claims,
                format_args!(",\"device_id\":\"{device_id}\""),
            );
        }
        claims.push('}');
        let claims = URL_SAFE_NO_PAD.encode(claims);
        let input = format!("{header}.{claims}");
        let signature = self.signing.sign(input.as_bytes());
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::IssuanceKey;

    #[test]
    fn issued_tokens_verify_against_the_published_jwks_shape() {
        let key = IssuanceKey::load(&write_test_key(), "test-key-1", 1).expect("key loads");
        let token = key
            .issue_token(
                "user-1",
                "axiusflow",
                "authorization_service",
                4_000_000_000,
                None,
                None,
            )
            .expect("token issues");
        let segments: Vec<&str> = token.split('.').collect();
        assert_eq!(segments.len(), 3);
        assert!(key.jwks_json().contains("test-key-1"));
    }

    fn write_test_key() -> String {
        let path =
            std::env::temp_dir().join(format!("axiusflow-test-signing-key-{}", std::process::id()));
        std::fs::write(&path, [7_u8; 32]).expect("test key writes");
        path.to_string_lossy().into_owned()
    }
}
