use crate::{
    LocalStorageError,
    model::{HistoryScope, SegmentEncryptionKey, SegmentIdentity, validate_identifier},
};
use chacha20poly1305::{
    Key, KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroize;

const SEGMENT_MAGIC: &[u8; 8] = b"AXHSEG01";
const SEGMENT_FORMAT_VERSION: u8 = 1;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const HEADER_BYTES: usize = SEGMENT_MAGIC.len() + 1 + NONCE_BYTES;
pub(crate) const SEGMENT_FILE_OVERHEAD_BYTES: usize = HEADER_BYTES + TAG_BYTES;

/// Stable catalog-index key loaded from the OS vault.
pub struct CatalogKey {
    key_id: String,
    bytes: [u8; 32],
}

impl CatalogKey {
    /// Creates a non-cloneable key container that zeroizes its bytes on drop.
    ///
    /// # Errors
    ///
    /// Returns an error when the non-secret vault key identifier is invalid.
    pub fn try_new(key_id: String, bytes: [u8; 32]) -> Result<Self, LocalStorageError> {
        validate_identifier("catalog_key_id", &key_id)?;
        Ok(Self { key_id, bytes })
    }

    /// Returns the non-secret OS-vault key identifier.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) const fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl fmt::Debug for CatalogKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogKey")
            .field("key_id", &self.key_id)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

impl Drop for CatalogKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

pub(crate) struct EncodedSegment {
    pub file: Vec<u8>,
    pub payload_checksum: [u8; 32],
    pub storage_checksum: [u8; 32],
}

pub(crate) fn encode_identity(identity: &SegmentIdentity) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(256);
    append_string(&mut encoded, &identity.scope.provider_id);
    append_string(&mut encoded, &identity.scope.account_id);
    append_string(&mut encoded, &identity.scope.entitlement_revision);
    append_string(&mut encoded, &identity.instrument_id);
    encoded.push(identity.data_kind.code());
    append_string(&mut encoded, &identity.resolution);
    encoded.extend_from_slice(&identity.range_start_unix_nanos.to_le_bytes());
    encoded.extend_from_slice(&identity.range_end_unix_nanos.to_le_bytes());
    encoded.extend_from_slice(&identity.source_revision.to_le_bytes());
    encoded.extend_from_slice(&identity.schema_revision.to_le_bytes());
    encoded.extend_from_slice(&identity.calendar_revision.to_le_bytes());
    encoded.extend_from_slice(&identity.adjustment_revision.to_le_bytes());
    encoded.extend_from_slice(&identity.correction_revision.to_le_bytes());
    encoded
}

pub(crate) fn identity_token(
    key: &CatalogKey,
    identity: &SegmentIdentity,
) -> Result<[u8; 32], LocalStorageError> {
    keyed_token(key, b"segment_identity_v1", &[&encode_identity(identity)])
}

pub(crate) fn catalog_key_verifier(key: &CatalogKey) -> Result<[u8; 32], LocalStorageError> {
    keyed_token(key, b"catalog_key_verifier_v1", &[])
}

pub(crate) fn segment_key_verifier(
    key: &SegmentEncryptionKey,
) -> Result<[u8; 32], LocalStorageError> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.bytes()).map_err(|_| {
        LocalStorageError::InvalidConfiguration("segment HMAC initialization failed")
    })?;
    mac.update(b"segment_key_verifier_v1");
    Ok(mac.finalize().into_bytes().into())
}

pub(crate) fn scope_tokens(
    key: &CatalogKey,
    scope: &HistoryScope,
) -> Result<ScopeTokens, LocalStorageError> {
    Ok(ScopeTokens {
        provider: keyed_token(key, b"provider_v1", &[scope.provider_id.as_bytes()])?,
        account: keyed_token(key, b"account_v1", &[scope.account_id.as_bytes()])?,
        entitlement: keyed_token(
            key,
            b"entitlement_v1",
            &[scope.entitlement_revision.as_bytes()],
        )?,
    })
}

pub(crate) fn instrument_token(
    key: &CatalogKey,
    instrument_id: &str,
) -> Result<[u8; 32], LocalStorageError> {
    keyed_token(key, b"instrument_v1", &[instrument_id.as_bytes()])
}

pub(crate) fn provider_token(
    key: &CatalogKey,
    provider_id: &str,
) -> Result<[u8; 32], LocalStorageError> {
    keyed_token(key, b"provider_v1", &[provider_id.as_bytes()])
}

pub(crate) fn account_token(
    key: &CatalogKey,
    account_id: &str,
) -> Result<[u8; 32], LocalStorageError> {
    keyed_token(key, b"account_v1", &[account_id.as_bytes()])
}

pub(crate) fn resolution_token(
    key: &CatalogKey,
    resolution: &str,
) -> Result<[u8; 32], LocalStorageError> {
    keyed_token(key, b"resolution_v1", &[resolution.as_bytes()])
}

fn keyed_token(
    key: &CatalogKey,
    domain: &[u8],
    values: &[&[u8]],
) -> Result<[u8; 32], LocalStorageError> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.bytes()).map_err(|_| {
        LocalStorageError::InvalidConfiguration("catalog HMAC initialization failed")
    })?;
    mac.update(domain);
    for value in values {
        mac.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        mac.update(value);
    }
    Ok(mac.finalize().into_bytes().into())
}

pub(crate) fn encrypt_segment(
    payload: &[u8],
    associated_data: &[u8],
    key: &SegmentEncryptionKey,
) -> Result<EncodedSegment, LocalStorageError> {
    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce_bytes)?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*key.bytes()));
    let nonce = XNonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: payload,
                aad: associated_data,
            },
        )
        .map_err(|_| LocalStorageError::AuthenticationFailed)?;
    let mut file = Vec::with_capacity(HEADER_BYTES.saturating_add(ciphertext.len()));
    file.extend_from_slice(SEGMENT_MAGIC);
    file.push(SEGMENT_FORMAT_VERSION);
    file.extend_from_slice(&nonce_bytes);
    file.extend_from_slice(&ciphertext);
    Ok(EncodedSegment {
        payload_checksum: checksum(payload),
        storage_checksum: checksum(&file),
        file,
    })
}

pub(crate) fn decrypt_segment(
    file: &[u8],
    associated_data: &[u8],
    key: &SegmentEncryptionKey,
) -> Result<Vec<u8>, LocalStorageError> {
    if file.len() < HEADER_BYTES.saturating_add(TAG_BYTES) {
        return Err(LocalStorageError::CorruptSegment("truncated header or tag"));
    }
    if &file[..SEGMENT_MAGIC.len()] != SEGMENT_MAGIC {
        return Err(LocalStorageError::CorruptSegment("invalid magic"));
    }
    if file[SEGMENT_MAGIC.len()] != SEGMENT_FORMAT_VERSION {
        return Err(LocalStorageError::CorruptSegment(
            "unsupported format version",
        ));
    }
    let nonce_start = SEGMENT_MAGIC.len() + 1;
    let nonce_end = nonce_start + NONCE_BYTES;
    let nonce_bytes: [u8; NONCE_BYTES] = file[nonce_start..nonce_end]
        .try_into()
        .map_err(|_| LocalStorageError::CorruptSegment("invalid nonce"))?;
    let nonce = XNonce::from(nonce_bytes);
    let cipher = XChaCha20Poly1305::new(&Key::from(*key.bytes()));
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &file[nonce_end..],
                aad: associated_data,
            },
        )
        .map_err(|_| LocalStorageError::AuthenticationFailed)
}

pub(crate) fn checksum(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn append_string(target: &mut Vec<u8>, value: &str) {
    target.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    target.extend_from_slice(value.as_bytes());
}

pub(crate) struct ScopeTokens {
    pub provider: [u8; 32],
    pub account: [u8; 32],
    pub entitlement: [u8; 32],
}
