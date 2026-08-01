//! Immutable Ed25519 JWKS snapshots parsed from bounded documents.

use crate::errors::KeySetSnapshotError;
use crate::identity::valid_identifier;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use core::fmt;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use std::collections::HashMap;

/// Maximum accepted serialized JWKS size.
pub const MAX_JWKS_JSON_BYTES: usize = 64 * 1_024;
/// Maximum verification keys accepted in one immutable snapshot.
pub const MAX_JWKS_KEYS: usize = 64;

const ED25519_PUBLIC_KEY_BYTES: usize = 32;

/// Immutable Ed25519 verification-key snapshot loaded by a service adapter.
pub struct Ed25519KeySetSnapshot {
    revision: u64,
    keys: HashMap<String, VerifyingKey>,
}

impl Ed25519KeySetSnapshot {
    /// Parses a bounded JWKS document into an immutable verification snapshot.
    ///
    /// Only unique, non-empty `kid` values with `kty=OKP`, `crv=Ed25519`,
    /// `alg=EdDSA`, verification-compatible key metadata, and 32-byte public
    /// key material are accepted.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero revision, oversized or malformed document,
    /// empty or oversized key set, duplicate key ID, invalid metadata, or
    /// invalid Ed25519 public-key material.
    pub fn try_from_jwks_json(revision: u64, jwks_json: &str) -> Result<Self, KeySetSnapshotError> {
        if revision == 0 {
            return Err(KeySetSnapshotError::ZeroRevision);
        }
        if jwks_json.len() > MAX_JWKS_JSON_BYTES {
            return Err(KeySetSnapshotError::DocumentTooLarge);
        }
        let document: RawJwks =
            serde_json::from_str(jwks_json).map_err(|_| KeySetSnapshotError::InvalidDocument)?;
        if document.keys.is_empty() {
            return Err(KeySetSnapshotError::EmptyKeySet);
        }
        if document.keys.len() > MAX_JWKS_KEYS {
            return Err(KeySetSnapshotError::TooManyKeys);
        }

        let mut keys = HashMap::with_capacity(document.keys.len());
        for raw_key in document.keys {
            let (key_id, decoding_key) = parse_ed25519_key(raw_key)?;
            if keys.insert(key_id, decoding_key).is_some() {
                return Err(KeySetSnapshotError::DuplicateKeyId);
            }
        }
        Ok(Self { revision, keys })
    }

    /// Returns the service-owned key-set revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the number of active and grace-period verification keys.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Returns whether this snapshot contains a key ID.
    #[must_use]
    pub fn contains_key_id(&self, key_id: &str) -> bool {
        self.keys.contains_key(key_id)
    }

    pub(crate) fn key(&self, key_id: &str) -> Option<&VerifyingKey> {
        self.keys.get(key_id)
    }
}

impl fmt::Debug for Ed25519KeySetSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ed25519KeySetSnapshot")
            .field("revision", &self.revision)
            .field("key_count", &self.keys.len())
            .finish_non_exhaustive()
    }
}

fn parse_ed25519_key(raw_key: RawJwk) -> Result<(String, VerifyingKey), KeySetSnapshotError> {
    if raw_key.kty.as_deref() != Some("OKP")
        || raw_key.crv.as_deref() != Some("Ed25519")
        || raw_key.alg.as_deref() != Some("EdDSA")
        || raw_key
            .key_use
            .as_deref()
            .is_some_and(|key_use| key_use != "sig")
        || raw_key.key_ops.as_ref().is_some_and(|operations| {
            operations.is_empty() || operations.iter().any(|operation| operation != "verify")
        })
    {
        return Err(KeySetSnapshotError::InvalidKeyMetadata);
    }

    let key_id = raw_key
        .kid
        .filter(|key_id| valid_identifier(key_id))
        .ok_or(KeySetSnapshotError::InvalidKeyMetadata)?;
    let encoded_x = raw_key
        .x
        .filter(|encoded_x| !encoded_x.is_empty() && encoded_x.len() <= 128)
        .ok_or(KeySetSnapshotError::InvalidKeyMaterial)?;
    let decoded_key = URL_SAFE_NO_PAD
        .decode(encoded_x)
        .map_err(|_| KeySetSnapshotError::InvalidKeyMaterial)?;
    let key_bytes: [u8; ED25519_PUBLIC_KEY_BYTES] = decoded_key
        .try_into()
        .map_err(|_| KeySetSnapshotError::InvalidKeyMaterial)?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| KeySetSnapshotError::InvalidKeyMaterial)?;
    if verifying_key.is_weak() {
        return Err(KeySetSnapshotError::InvalidKeyMaterial);
    }
    Ok((key_id, verifying_key))
}

#[derive(Debug, Deserialize)]
struct RawJwks {
    keys: Vec<RawJwk>,
}

#[derive(Debug, Deserialize)]
struct RawJwk {
    kty: Option<String>,
    crv: Option<String>,
    alg: Option<String>,
    kid: Option<String>,
    x: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    key_ops: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::Ed25519KeySetSnapshot;
    use crate::errors::KeySetSnapshotError;
    use crate::test_fixture::jwks;
    use serde_json::json;

    #[test]
    fn jwks_snapshot_rejects_invalid_revision_metadata_and_duplicate_keys() {
        assert!(matches!(
            Ed25519KeySetSnapshot::try_from_jwks_json(0, &jwks("key-1")),
            Err(KeySetSnapshotError::ZeroRevision)
        ));
        assert!(matches!(
            Ed25519KeySetSnapshot::try_from_jwks_json(1, r#"{"keys":[]}"#),
            Err(KeySetSnapshotError::EmptyKeySet)
        ));

        let key = serde_json::from_str::<serde_json::Value>(&jwks("key-1"))
            .expect("test JWKS parses")["keys"][0]
            .clone();
        let duplicate = json!({"keys": [key.clone(), key]}).to_string();
        assert!(matches!(
            Ed25519KeySetSnapshot::try_from_jwks_json(1, &duplicate),
            Err(KeySetSnapshotError::DuplicateKeyId)
        ));
    }
}
