//! Signed update verification and anti-downgrade rollback state.

#[cfg(unix)]
mod activation;

#[cfg(unix)]
pub use activation::{DurableUpdateActivator, UpdateActivationError};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{error::Error, fmt, io::Read};

pub const MAX_UPDATE_MANIFEST_BYTES: usize = 4 * 1_024;
pub const MAX_UPDATE_VERSION_BYTES: usize = 64;
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 8 * 1_024 * 1_024 * 1_024;
pub const MAX_UPDATE_KEY_ROTATION_BYTES: usize = 1_024;
pub const MAX_UPDATE_SIGNING_KEYS: usize = 16;

const UPDATE_MANIFEST_SCHEMA_VERSION: u32 = 1;
const UPDATE_KEY_ROTATION_SCHEMA_VERSION: u32 = 1;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
#[cfg(unix)]
const ED25519_SIGNATURE_BYTES: usize = 64;
const SHA256_BYTES: usize = 32;

/// A signature-verified update manifest with bounded artifact metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateManifest {
    release_sequence: u64,
    version: String,
    artifact_bytes: u64,
    artifact_sha256: [u8; SHA256_BYTES],
    signing_key_identity: [u8; SHA256_BYTES],
    trust_domain_identity: [u8; SHA256_BYTES],
    verifier_binding: [u8; SHA256_BYTES],
    #[cfg(unix)]
    signed_json: Vec<u8>,
    #[cfg(unix)]
    signature: [u8; ED25519_SIGNATURE_BYTES],
}

impl UpdateManifest {
    #[must_use]
    pub const fn release_sequence(&self) -> u64 {
        self.release_sequence
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub const fn artifact_bytes(&self) -> u64 {
        self.artifact_bytes
    }
}

/// A release whose manifest signature and artifact digest were both verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledRelease {
    manifest: UpdateManifest,
}

impl InstalledRelease {
    #[must_use]
    pub const fn manifest(&self) -> &UpdateManifest {
        &self.manifest
    }
}

/// Verifies one immutable update signing key and an anti-downgrade sequence floor.
#[derive(Clone)]
pub struct SignedUpdateVerifier {
    verifying_key: VerifyingKey,
    minimum_release_sequence: u64,
    trust_domain_identity: [u8; SHA256_BYTES],
    authorized_key_identities: Vec<[u8; SHA256_BYTES]>,
}

impl SignedUpdateVerifier {
    /// Creates an update verifier for an independently managed Ed25519 public key.
    ///
    /// # Errors
    ///
    /// Returns an error when the public key is malformed or weak.
    pub fn try_new(
        verifying_key: [u8; ED25519_PUBLIC_KEY_BYTES],
        minimum_release_sequence: u64,
    ) -> Result<Self, SignedUpdateError> {
        let verifying_key = VerifyingKey::from_bytes(&verifying_key)
            .map_err(|_| SignedUpdateError::InvalidVerificationKey)?;
        if verifying_key.is_weak() {
            return Err(SignedUpdateError::InvalidVerificationKey);
        }
        let trust_domain_identity = signing_key_identity(&verifying_key);
        Ok(Self {
            verifying_key,
            minimum_release_sequence,
            trust_domain_identity,
            authorized_key_identities: vec![trust_domain_identity],
        })
    }

    /// Returns the domain-separated identity callers place in a successor-key
    /// authorization statement as `current_key_identity`.
    #[must_use]
    pub fn current_signing_key_identity(&self) -> [u8; SHA256_BYTES] {
        self.signing_key_identity()
    }

    /// Returns the anti-downgrade floor a successor-key authorization may preserve
    /// or raise, but never lower.
    #[must_use]
    pub const fn minimum_release_sequence(&self) -> u64 {
        self.minimum_release_sequence
    }

    /// Authorizes a successor signing key using an exact bounded statement signed by
    /// the current key. The new verifier retains the original trust-domain identity,
    /// allowing in-memory rollback to the predecessor across the authorized rotation.
    /// The strict JSON statement contains `schema_version`, base64url-no-pad
    /// `current_key_identity`, base64url-no-pad `next_verifying_key`, and
    /// `minimum_release_sequence` fields, with no additional fields accepted.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized, malformed, unsigned, stale, weak, reused,
    /// or anti-downgrade-floor-lowering rotation statement.
    pub fn authorize_rotation(
        &self,
        rotation_json: &[u8],
        signature: &[u8],
    ) -> Result<Self, SignedUpdateError> {
        if rotation_json.is_empty() || rotation_json.len() > MAX_UPDATE_KEY_ROTATION_BYTES {
            return Err(SignedUpdateError::InvalidKeyRotationSize);
        }
        let signature = Signature::from_slice(signature)
            .map_err(|_| SignedUpdateError::InvalidKeyRotationSignature)?;
        self.verifying_key
            .verify_strict(rotation_json, &signature)
            .map_err(|_| SignedUpdateError::InvalidKeyRotationSignature)?;
        let raw: RawUpdateKeyRotation = serde_json::from_slice(rotation_json)
            .map_err(|_| SignedUpdateError::InvalidKeyRotation)?;
        if raw.schema_version != UPDATE_KEY_ROTATION_SCHEMA_VERSION {
            return Err(SignedUpdateError::UnsupportedKeyRotationSchema);
        }
        let current_key_identity = decode_sha256(&raw.current_key_identity)
            .map_err(|()| SignedUpdateError::InvalidKeyRotation)?;
        if current_key_identity != self.signing_key_identity() {
            return Err(SignedUpdateError::KeyRotationCurrentKeyMismatch);
        }
        if raw.minimum_release_sequence < self.minimum_release_sequence {
            return Err(SignedUpdateError::KeyRotationLowersReleaseFloor);
        }
        if self.authorized_key_identities.len() >= MAX_UPDATE_SIGNING_KEYS {
            return Err(SignedUpdateError::KeyRotationChainFull);
        }
        let next_key_bytes = URL_SAFE_NO_PAD
            .decode(raw.next_verifying_key)
            .map_err(|_| SignedUpdateError::InvalidKeyRotation)?;
        let next_key_bytes = next_key_bytes
            .try_into()
            .map_err(|_| SignedUpdateError::InvalidKeyRotation)?;
        let verifying_key = VerifyingKey::from_bytes(&next_key_bytes)
            .map_err(|_| SignedUpdateError::InvalidVerificationKey)?;
        if verifying_key.is_weak() {
            return Err(SignedUpdateError::InvalidVerificationKey);
        }
        let next_key_identity = signing_key_identity(&verifying_key);
        if self.authorized_key_identities.contains(&next_key_identity) {
            return Err(SignedUpdateError::KeyRotationReusesSigningKey);
        }
        let mut authorized_key_identities = self.authorized_key_identities.clone();
        authorized_key_identities.push(next_key_identity);
        Ok(Self {
            verifying_key,
            minimum_release_sequence: raw.minimum_release_sequence,
            trust_domain_identity: self.trust_domain_identity,
            authorized_key_identities,
        })
    }

    /// Verifies the exact bounded manifest bytes before parsing their strict schema.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, unsigned, or non-increasing manifests.
    pub fn verify_manifest(
        &self,
        manifest_json: &[u8],
        signature: &[u8],
    ) -> Result<UpdateManifest, SignedUpdateError> {
        self.verify_manifest_with_floor(manifest_json, signature, true)
    }

    #[cfg(unix)]
    fn verify_persisted_manifest(
        &self,
        manifest_json: &[u8],
        signature: &[u8],
    ) -> Result<UpdateManifest, SignedUpdateError> {
        self.verify_manifest_with_floor(manifest_json, signature, false)
    }

    fn verify_manifest_with_floor(
        &self,
        manifest_json: &[u8],
        signature: &[u8],
        enforce_floor: bool,
    ) -> Result<UpdateManifest, SignedUpdateError> {
        if manifest_json.is_empty() || manifest_json.len() > MAX_UPDATE_MANIFEST_BYTES {
            return Err(SignedUpdateError::InvalidManifestSize);
        }
        let signature = Signature::from_slice(signature)
            .map_err(|_| SignedUpdateError::InvalidManifestSignature)?;
        self.verifying_key
            .verify_strict(manifest_json, &signature)
            .map_err(|_| SignedUpdateError::InvalidManifestSignature)?;

        let raw: RawUpdateManifest = serde_json::from_slice(manifest_json)
            .map_err(|_| SignedUpdateError::InvalidManifest)?;
        if raw.schema_version != UPDATE_MANIFEST_SCHEMA_VERSION {
            return Err(SignedUpdateError::UnsupportedManifestSchema);
        }
        if raw.release_sequence == 0 {
            return Err(SignedUpdateError::ReleaseSequenceNotIncreasing);
        }
        if enforce_floor && raw.release_sequence <= self.minimum_release_sequence {
            return Err(SignedUpdateError::ReleaseSequenceNotIncreasing);
        }
        if !valid_version(&raw.version) {
            return Err(SignedUpdateError::InvalidVersion);
        }
        if raw.artifact_bytes == 0 || raw.artifact_bytes > MAX_UPDATE_ARTIFACT_BYTES {
            return Err(SignedUpdateError::InvalidArtifactSize);
        }
        let digest = URL_SAFE_NO_PAD
            .decode(raw.artifact_sha256)
            .map_err(|_| SignedUpdateError::InvalidArtifactDigest)?;
        let artifact_sha256 = digest
            .try_into()
            .map_err(|_| SignedUpdateError::InvalidArtifactDigest)?;

        Ok(UpdateManifest {
            release_sequence: raw.release_sequence,
            version: raw.version,
            artifact_bytes: raw.artifact_bytes,
            artifact_sha256,
            signing_key_identity: self.signing_key_identity(),
            trust_domain_identity: self.trust_domain_identity,
            verifier_binding: self.binding(),
            #[cfg(unix)]
            signed_json: manifest_json.to_vec(),
            #[cfg(unix)]
            signature: signature.to_bytes(),
        })
    }

    /// Streams and verifies the exact artifact described by a verified manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for read failures, truncation, trailing bytes, or digest mismatch.
    pub fn verify_artifact(
        &self,
        manifest: UpdateManifest,
        mut artifact: impl Read,
    ) -> Result<InstalledRelease, SignedUpdateError> {
        if manifest.verifier_binding != self.binding() {
            return Err(SignedUpdateError::ManifestVerifierMismatch);
        }
        let mut digest = Sha256::new();
        let mut remaining = manifest.artifact_bytes;
        let mut buffer = vec![0_u8; 64 * 1_024];

        while remaining > 0 {
            let requested = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| SignedUpdateError::InvalidArtifactSize)?;
            let read = read_retry_interrupted(&mut artifact, &mut buffer[..requested])?;
            if read == 0 {
                return Err(SignedUpdateError::ArtifactTruncated);
            }
            digest.update(&buffer[..read]);
            remaining -= u64::try_from(read).map_err(|_| SignedUpdateError::InvalidArtifactSize)?;
        }

        let mut trailing = [0_u8; 1];
        if read_retry_interrupted(&mut artifact, &mut trailing)? != 0 {
            return Err(SignedUpdateError::ArtifactHasTrailingBytes);
        }
        let actual: [u8; SHA256_BYTES] = digest.finalize().into();
        if actual != manifest.artifact_sha256 {
            return Err(SignedUpdateError::ArtifactDigestMismatch);
        }
        Ok(InstalledRelease { manifest })
    }

    fn binding(&self) -> [u8; SHA256_BYTES] {
        let mut digest = Sha256::new();
        digest.update(b"axiusflow-signed-update-verifier-v1\0");
        digest.update(self.trust_domain_identity);
        digest.update(self.signing_key_identity());
        digest.update(self.minimum_release_sequence.to_be_bytes());
        digest.finalize().into()
    }

    fn signing_key_identity(&self) -> [u8; SHA256_BYTES] {
        signing_key_identity(&self.verifying_key)
    }
}

impl fmt::Debug for SignedUpdateVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignedUpdateVerifier")
            .field("minimum_release_sequence", &self.minimum_release_sequence)
            .finish_non_exhaustive()
    }
}

/// In-memory activation state. Durable adapters persist the same sequence floor and slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateRollbackState {
    active: InstalledRelease,
    rollback: Option<InstalledRelease>,
    minimum_release_sequence: u64,
}

impl UpdateRollbackState {
    #[must_use]
    pub const fn new(active: InstalledRelease) -> Self {
        let minimum_release_sequence = active.manifest.release_sequence;
        Self {
            active,
            rollback: None,
            minimum_release_sequence,
        }
    }

    /// Restores persisted activation state without lowering its anti-downgrade floor.
    ///
    /// # Errors
    ///
    /// Returns an error when the floor is below the active release or the retained
    /// rollback release is not older than the active release.
    pub fn try_restore(
        active: InstalledRelease,
        rollback: Option<InstalledRelease>,
        minimum_release_sequence: u64,
    ) -> Result<Self, SignedUpdateError> {
        if minimum_release_sequence < active.manifest.release_sequence
            || (minimum_release_sequence > active.manifest.release_sequence && rollback.is_some())
            || rollback.as_ref().is_some_and(|release| {
                release.manifest.release_sequence >= active.manifest.release_sequence
                    || release.manifest.trust_domain_identity
                        != active.manifest.trust_domain_identity
            })
        {
            return Err(SignedUpdateError::InvalidRollbackState);
        }
        Ok(Self {
            active,
            rollback,
            minimum_release_sequence,
        })
    }

    #[must_use]
    pub const fn active(&self) -> &InstalledRelease {
        &self.active
    }

    #[must_use]
    pub const fn minimum_release_sequence(&self) -> u64 {
        self.minimum_release_sequence
    }

    #[must_use]
    pub const fn rollback_available(&self) -> bool {
        self.rollback.is_some()
    }

    /// Activates a newer verified release and retains exactly one verified predecessor.
    ///
    /// # Errors
    ///
    /// Returns an error when the candidate does not advance the durable sequence floor.
    pub fn activate(&mut self, candidate: InstalledRelease) -> Result<(), SignedUpdateError> {
        if candidate.manifest.trust_domain_identity != self.active.manifest.trust_domain_identity {
            return Err(SignedUpdateError::ReleaseTrustDomainMismatch);
        }
        if candidate.manifest.release_sequence <= self.minimum_release_sequence {
            return Err(SignedUpdateError::ReleaseSequenceNotIncreasing);
        }
        self.minimum_release_sequence = candidate.manifest.release_sequence;
        self.rollback = Some(std::mem::replace(&mut self.active, candidate));
        Ok(())
    }

    /// Restores the locally verified predecessor without lowering the sequence floor.
    ///
    /// # Errors
    ///
    /// Returns an error when no predecessor is retained.
    pub fn rollback(&mut self) -> Result<(), SignedUpdateError> {
        self.active = self
            .rollback
            .take()
            .ok_or(SignedUpdateError::RollbackUnavailable)?;
        Ok(())
    }
}

/// Signed update verification and rollback errors.
#[derive(Debug)]
pub enum SignedUpdateError {
    InvalidVerificationKey,
    InvalidManifestSize,
    InvalidManifestSignature,
    InvalidManifest,
    UnsupportedManifestSchema,
    ReleaseSequenceNotIncreasing,
    InvalidVersion,
    InvalidArtifactSize,
    InvalidArtifactDigest,
    ManifestVerifierMismatch,
    ReleaseTrustDomainMismatch,
    ArtifactRead(std::io::Error),
    ArtifactTruncated,
    ArtifactHasTrailingBytes,
    ArtifactDigestMismatch,
    InvalidRollbackState,
    RollbackUnavailable,
    InvalidKeyRotationSize,
    InvalidKeyRotationSignature,
    InvalidKeyRotation,
    UnsupportedKeyRotationSchema,
    KeyRotationCurrentKeyMismatch,
    KeyRotationLowersReleaseFloor,
    KeyRotationReusesSigningKey,
    KeyRotationChainFull,
    RotationPersistenceUnsupported,
}

impl fmt::Display for SignedUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVerificationKey => {
                formatter.write_str("update verification key is invalid")
            }
            Self::InvalidManifestSize => formatter.write_str("update manifest size is invalid"),
            Self::InvalidManifestSignature => {
                formatter.write_str("update manifest signature is invalid")
            }
            Self::InvalidManifest => formatter.write_str("update manifest is invalid"),
            Self::UnsupportedManifestSchema => {
                formatter.write_str("update manifest schema is unsupported")
            }
            Self::ReleaseSequenceNotIncreasing => formatter
                .write_str("update release sequence does not advance the anti-downgrade floor"),
            Self::InvalidVersion => formatter.write_str("update version is invalid"),
            Self::InvalidArtifactSize => formatter.write_str("update artifact size is invalid"),
            Self::InvalidArtifactDigest => formatter.write_str("update artifact digest is invalid"),
            Self::ManifestVerifierMismatch => {
                formatter.write_str("update manifest was authenticated by another verifier")
            }
            Self::ReleaseTrustDomainMismatch => {
                formatter.write_str("update release belongs to another signing-key trust domain")
            }
            Self::ArtifactRead(error) => write!(formatter, "update artifact read failed: {error}"),
            Self::ArtifactTruncated => formatter.write_str("update artifact is truncated"),
            Self::ArtifactHasTrailingBytes => {
                formatter.write_str("update artifact has trailing bytes")
            }
            Self::ArtifactDigestMismatch => {
                formatter.write_str("update artifact digest does not match")
            }
            Self::InvalidRollbackState => {
                formatter.write_str("persisted update rollback state is invalid")
            }
            Self::RollbackUnavailable => {
                formatter.write_str("no verified rollback release is available")
            }
            Self::InvalidKeyRotationSize => {
                formatter.write_str("update signing-key rotation size is invalid")
            }
            Self::InvalidKeyRotationSignature => {
                formatter.write_str("update signing-key rotation signature is invalid")
            }
            Self::InvalidKeyRotation => {
                formatter.write_str("update signing-key rotation is invalid")
            }
            Self::UnsupportedKeyRotationSchema => {
                formatter.write_str("update signing-key rotation schema is unsupported")
            }
            Self::KeyRotationCurrentKeyMismatch => {
                formatter.write_str("update signing-key rotation names another current key")
            }
            Self::KeyRotationLowersReleaseFloor => {
                formatter.write_str("update signing-key rotation lowers the anti-downgrade floor")
            }
            Self::KeyRotationReusesSigningKey => {
                formatter.write_str("update signing-key rotation reuses an authorized key")
            }
            Self::KeyRotationChainFull => {
                formatter.write_str("update signing-key rotation chain is full")
            }
            Self::RotationPersistenceUnsupported => formatter
                .write_str("durable activation across update signing-key rotation is unsupported"),
        }
    }
}

impl Error for SignedUpdateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ArtifactRead(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUpdateManifest {
    schema_version: u32,
    release_sequence: u64,
    version: String,
    artifact_bytes: u64,
    artifact_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUpdateKeyRotation {
    schema_version: u32,
    current_key_identity: String,
    next_verifying_key: String,
    minimum_release_sequence: u64,
}

fn signing_key_identity(verifying_key: &VerifyingKey) -> [u8; SHA256_BYTES] {
    let mut digest = Sha256::new();
    digest.update(b"axiusflow-signed-update-key-v1\0");
    digest.update(verifying_key.as_bytes());
    digest.finalize().into()
}

fn decode_sha256(encoded: &str) -> Result<[u8; SHA256_BYTES], ()> {
    URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(())
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= MAX_UPDATE_VERSION_BYTES
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
}

fn read_retry_interrupted(
    reader: &mut impl Read,
    buffer: &mut [u8],
) -> Result<usize, SignedUpdateError> {
    loop {
        match reader.read(buffer) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(SignedUpdateError::ArtifactRead(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_UPDATE_KEY_ROTATION_BYTES, MAX_UPDATE_MANIFEST_BYTES, SignedUpdateError,
        SignedUpdateVerifier, UpdateRollbackState, signing_key_identity,
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};
    use std::io::{self, Cursor, Read};

    struct InterruptedReader {
        inner: Cursor<Vec<u8>>,
        interrupt_payload: bool,
        interrupt_trailing_probe: bool,
    }

    impl Read for InterruptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.interrupt_payload {
                self.interrupt_payload = false;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if self.interrupt_trailing_probe
                && self.inner.position() == self.inner.get_ref().len() as u64
            {
                self.interrupt_trailing_probe = false;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.inner.read(buffer)
        }
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[41_u8; 32])
    }

    fn signed_manifest_with_key(
        signing_key: &SigningKey,
        release_sequence: u64,
        version: &str,
        artifact: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(artifact));
        let json = format!(
            r#"{{"schema_version":1,"release_sequence":{release_sequence},"version":"{version}","artifact_bytes":{},"artifact_sha256":"{digest}"}}"#,
            artifact.len()
        )
        .into_bytes();
        let signature = signing_key.sign(&json).to_bytes().to_vec();
        (json, signature)
    }

    fn signed_manifest(
        release_sequence: u64,
        version: &str,
        artifact: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        signed_manifest_with_key(&signing_key(), release_sequence, version, artifact)
    }

    fn signed_rotation(
        current_key: &SigningKey,
        next_key: &SigningKey,
        minimum_release_sequence: u64,
    ) -> (Vec<u8>, Vec<u8>) {
        let current_key_identity =
            URL_SAFE_NO_PAD.encode(signing_key_identity(&current_key.verifying_key()));
        let next_verifying_key = URL_SAFE_NO_PAD.encode(next_key.verifying_key().to_bytes());
        let json = format!(
            r#"{{"schema_version":1,"current_key_identity":"{current_key_identity}","next_verifying_key":"{next_verifying_key}","minimum_release_sequence":{minimum_release_sequence}}}"#
        )
        .into_bytes();
        let signature = current_key.sign(&json).to_bytes().to_vec();
        (json, signature)
    }

    fn verifier(floor: u64) -> SignedUpdateVerifier {
        SignedUpdateVerifier::try_new(signing_key().verifying_key().to_bytes(), floor)
            .expect("test verification key is valid")
    }

    fn installed(release_sequence: u64, artifact: &[u8]) -> super::InstalledRelease {
        let (json, signature) = signed_manifest(release_sequence, "1.2.3", artifact);
        let manifest = verifier(release_sequence - 1)
            .verify_manifest(&json, &signature)
            .expect("signed manifest is valid");
        verifier(release_sequence - 1)
            .verify_artifact(manifest, Cursor::new(artifact))
            .expect("artifact matches manifest")
    }

    fn installed_with_key(
        signing_key: &SigningKey,
        release_sequence: u64,
        artifact: &[u8],
    ) -> super::InstalledRelease {
        let (json, signature) =
            signed_manifest_with_key(signing_key, release_sequence, "1.2.3", artifact);
        let key_verifier = SignedUpdateVerifier::try_new(
            signing_key.verifying_key().to_bytes(),
            release_sequence - 1,
        )
        .expect("test verification key is valid");
        let manifest = key_verifier
            .verify_manifest(&json, &signature)
            .expect("signed manifest is valid");
        key_verifier
            .verify_artifact(manifest, Cursor::new(artifact))
            .expect("artifact matches manifest")
    }

    #[test]
    fn independently_signed_manifest_and_streamed_artifact_are_verified() {
        let artifact = b"native-release-artifact\0with-binary-data";
        let (json, signature) = signed_manifest(12, "2.0.0-rc_1", artifact);
        let verifier = verifier(11);
        let manifest = verifier
            .verify_manifest(&json, &signature)
            .expect("signature and schema are valid");

        assert_eq!(manifest.release_sequence(), 12);
        assert_eq!(manifest.version(), "2.0.0-rc_1");
        assert_eq!(manifest.artifact_bytes(), artifact.len() as u64);
        assert!(
            verifier
                .verify_artifact(manifest, Cursor::new(artifact))
                .is_ok()
        );
    }

    #[test]
    fn manifest_verification_rejects_tampering_unknown_fields_and_downgrades() {
        let artifact = b"release";
        let (json, signature) = signed_manifest(12, "2.0.0", artifact);
        let update_verifier = verifier(11);
        let mut tampered = json.clone();
        tampered[0] ^= 1;
        assert!(matches!(
            update_verifier.verify_manifest(&tampered, &signature),
            Err(SignedUpdateError::InvalidManifestSignature)
        ));

        let unknown = br#"{"schema_version":1,"release_sequence":12,"version":"2.0.0","artifact_bytes":7,"artifact_sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","extra":true}"#;
        let unknown_signature = signing_key().sign(unknown).to_bytes();
        assert!(matches!(
            update_verifier.verify_manifest(unknown, &unknown_signature),
            Err(SignedUpdateError::InvalidManifest)
        ));
        assert!(matches!(
            verifier(12).verify_manifest(&json, &signature),
            Err(SignedUpdateError::ReleaseSequenceNotIncreasing)
        ));
        let oversized = vec![b' '; MAX_UPDATE_MANIFEST_BYTES + 1];
        assert!(matches!(
            update_verifier.verify_manifest(&oversized, &signature),
            Err(SignedUpdateError::InvalidManifestSize)
        ));
    }

    #[test]
    fn artifact_verification_rejects_truncation_trailing_bytes_and_digest_changes() {
        let artifact = b"release";
        let (json, signature) = signed_manifest(20, "3.0.0", artifact);
        let verifier = verifier(19);
        let manifest = verifier
            .verify_manifest(&json, &signature)
            .expect("signed manifest is valid");

        assert!(matches!(
            verifier.verify_artifact(manifest.clone(), Cursor::new(&artifact[..3])),
            Err(SignedUpdateError::ArtifactTruncated)
        ));
        assert!(matches!(
            verifier.verify_artifact(manifest.clone(), Cursor::new(b"release-extra")),
            Err(SignedUpdateError::ArtifactHasTrailingBytes)
        ));
        assert!(matches!(
            verifier.verify_artifact(manifest, Cursor::new(b"changed")),
            Err(SignedUpdateError::ArtifactDigestMismatch)
        ));
    }

    #[test]
    fn artifact_verification_retries_interrupted_payload_and_trailing_reads() {
        let artifact = b"release";
        let (json, signature) = signed_manifest(21, "3.0.1", artifact);
        let verifier = verifier(20);
        let manifest = verifier
            .verify_manifest(&json, &signature)
            .expect("signed manifest is valid");
        let reader = InterruptedReader {
            inner: Cursor::new(artifact.to_vec()),
            interrupt_payload: true,
            interrupt_trailing_probe: true,
        };

        assert!(verifier.verify_artifact(manifest, reader).is_ok());
    }

    #[test]
    fn artifact_verification_rejects_manifests_from_another_key_or_floor() {
        let artifact = b"release";
        let untrusted_key = SigningKey::from_bytes(&[73_u8; 32]);
        let (untrusted_json, untrusted_signature) =
            signed_manifest_with_key(&untrusted_key, 22, "3.0.2", artifact);
        let untrusted_verifier =
            SignedUpdateVerifier::try_new(untrusted_key.verifying_key().to_bytes(), 21)
                .expect("untrusted test verification key is valid");
        let untrusted_manifest = untrusted_verifier
            .verify_manifest(&untrusted_json, &untrusted_signature)
            .expect("untrusted verifier authenticates its own manifest");
        assert!(matches!(
            verifier(21).verify_artifact(untrusted_manifest, Cursor::new(artifact)),
            Err(SignedUpdateError::ManifestVerifierMismatch)
        ));

        let (json, signature) = signed_manifest(22, "3.0.2", artifact);
        let lower_floor_manifest = verifier(20)
            .verify_manifest(&json, &signature)
            .expect("lower-floor verifier authenticates the manifest");
        assert!(matches!(
            verifier(21).verify_artifact(lower_floor_manifest, Cursor::new(artifact)),
            Err(SignedUpdateError::ManifestVerifierMismatch)
        ));
    }

    #[test]
    fn authorized_rotation_preserves_trust_domain_and_cross_key_rollback() {
        let successor_key = SigningKey::from_bytes(&[59_u8; 32]);
        let (rotation, rotation_signature) = signed_rotation(&signing_key(), &successor_key, 10);
        let successor_verifier = verifier(10)
            .authorize_rotation(&rotation, &rotation_signature)
            .expect("current signing key authorizes its successor");
        let artifact = b"successor-release";
        let (manifest_json, manifest_signature) =
            signed_manifest_with_key(&successor_key, 11, "2.0.0", artifact);
        let manifest = successor_verifier
            .verify_manifest(&manifest_json, &manifest_signature)
            .expect("successor key authenticates the next release");
        let successor_release = successor_verifier
            .verify_artifact(manifest, Cursor::new(artifact))
            .expect("successor artifact matches its manifest");
        let predecessor = installed(10, b"predecessor-release");
        let mut state = UpdateRollbackState::new(predecessor.clone());

        state
            .activate(successor_release)
            .expect("authorized successor remains in the root trust domain");
        state
            .rollback()
            .expect("rollback crosses the authorized key boundary");
        assert_eq!(state.active(), &predecessor);
        assert_eq!(state.minimum_release_sequence(), 11);
        assert!(matches!(
            verifier(10).verify_manifest(&manifest_json, &manifest_signature),
            Err(SignedUpdateError::InvalidManifestSignature)
        ));
    }

    #[test]
    fn rotation_rejects_tampering_stale_floors_and_key_reuse() {
        let successor_key = SigningKey::from_bytes(&[61_u8; 32]);
        let current_verifier = verifier(10);
        assert_eq!(
            current_verifier.current_signing_key_identity(),
            signing_key_identity(&signing_key().verifying_key())
        );
        assert_eq!(current_verifier.minimum_release_sequence(), 10);
        let (rotation, signature) = signed_rotation(&signing_key(), &successor_key, 10);
        let mut tampered = rotation.clone();
        tampered[0] ^= 1;
        assert!(matches!(
            current_verifier.authorize_rotation(&tampered, &signature),
            Err(SignedUpdateError::InvalidKeyRotationSignature)
        ));

        let (stale_rotation, stale_signature) = signed_rotation(&signing_key(), &successor_key, 9);
        assert!(matches!(
            current_verifier.authorize_rotation(&stale_rotation, &stale_signature),
            Err(SignedUpdateError::KeyRotationLowersReleaseFloor)
        ));

        let (reused_rotation, reused_signature) =
            signed_rotation(&signing_key(), &signing_key(), 10);
        assert!(matches!(
            current_verifier.authorize_rotation(&reused_rotation, &reused_signature),
            Err(SignedUpdateError::KeyRotationReusesSigningKey)
        ));

        let oversized = vec![b' '; MAX_UPDATE_KEY_ROTATION_BYTES + 1];
        assert!(matches!(
            current_verifier.authorize_rotation(&oversized, &signature),
            Err(SignedUpdateError::InvalidKeyRotationSize)
        ));
    }

    #[test]
    fn rollback_restores_only_the_verified_predecessor_without_lowering_the_floor() {
        let release_10 = installed(10, b"release-10");
        let release_11 = installed(11, b"release-11");
        let mut state = UpdateRollbackState::new(release_10.clone());

        state
            .activate(release_11.clone())
            .expect("new release advances the floor");
        assert_eq!(state.active(), &release_11);
        assert_eq!(state.minimum_release_sequence(), 11);
        assert!(state.rollback_available());
        state.rollback().expect("verified predecessor is retained");
        assert_eq!(state.active(), &release_10);
        assert_eq!(state.minimum_release_sequence(), 11);
        assert!(!state.rollback_available());
        assert!(matches!(
            state.rollback(),
            Err(SignedUpdateError::RollbackUnavailable)
        ));
        assert!(matches!(
            state.activate(release_11),
            Err(SignedUpdateError::ReleaseSequenceNotIncreasing)
        ));

        let restored = UpdateRollbackState::try_restore(release_10.clone(), None, 11)
            .expect("persisted rollback state retains the higher floor");
        assert_eq!(restored.active(), &release_10);
        assert_eq!(restored.minimum_release_sequence(), 11);
        assert!(matches!(
            UpdateRollbackState::try_restore(release_10.clone(), None, 9),
            Err(SignedUpdateError::InvalidRollbackState)
        ));
        assert!(matches!(
            UpdateRollbackState::try_restore(release_10, Some(installed(11, b"release-11")), 11),
            Err(SignedUpdateError::InvalidRollbackState)
        ));
    }

    #[test]
    fn rollback_state_confines_releases_to_one_signing_key_trust_domain() {
        let other_key = SigningKey::from_bytes(&[97_u8; 32]);
        let trusted_10 = installed(10, b"release-10");
        let trusted_9 = installed(9, b"release-9");
        let foreign_11 = installed_with_key(&other_key, 11, b"foreign-release-11");
        let foreign_9 = installed_with_key(&other_key, 9, b"foreign-release-9");
        let mut state = UpdateRollbackState::new(trusted_10.clone());

        assert!(matches!(
            state.activate(foreign_11),
            Err(SignedUpdateError::ReleaseTrustDomainMismatch)
        ));
        assert!(matches!(
            UpdateRollbackState::try_restore(trusted_10.clone(), Some(foreign_9), 10),
            Err(SignedUpdateError::InvalidRollbackState)
        ));
        assert!(matches!(
            UpdateRollbackState::try_restore(trusted_10.clone(), Some(trusted_9.clone()), 11),
            Err(SignedUpdateError::InvalidRollbackState)
        ));
        assert!(UpdateRollbackState::try_restore(trusted_10, Some(trusted_9), 10).is_ok());
    }
}
