//! The release signing key: a local key file, or an AWS KMS Ed25519 key whose private half
//! never leaves KMS. KMS is reached only through the AWS CLI's `kms get-public-key` and
//! `kms sign`, with the maintainer's own AWS credentials; nothing here publishes.

use std::{
    fmt,
    process::{Command, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};

/// The DER prefix of an Ed25519 `SubjectPublicKeyInfo` (RFC 8410); the 32 key bytes follow.
const ED25519_PUBLIC_KEY_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const MAXIMUM_REPORTED_CLI_ERROR_CHARS: usize = 512;

/// Why the release signing key could not be loaded or used. It never carries key material.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignerError(String);

impl SignerError {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl fmt::Display for SignerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SignerError {}

/// The key that signs release manifests and block plans.
pub enum ReleaseSigner {
    KeyFile(SigningKey),
    Kms(KmsKey),
}

impl ReleaseSigner {
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        match self {
            Self::KeyFile(key) => key.verifying_key(),
            Self::Kms(key) => key.verifying_key,
        }
    }

    /// Signs `message` with pure Ed25519. A KMS signature is checked against the key's public
    /// half before it is returned.
    ///
    /// # Errors
    /// Returns why KMS did not produce a valid signature.
    pub fn sign(&self, message: &[u8]) -> Result<Signature, SignerError> {
        match self {
            Self::KeyFile(key) => Ok(key.sign(message)),
            Self::Kms(key) => key.sign(message),
        }
    }
}

/// An AWS KMS `ECC_NIST_EDWARDS25519` signing key named by its full key ARN.
pub struct KmsKey {
    arn: String,
    region: String,
    verifying_key: VerifyingKey,
}

impl KmsKey {
    /// Reads the key's public half from KMS and checks that it is an Ed25519 signing key.
    ///
    /// # Errors
    /// Returns why the ARN is unusable or KMS did not describe an Ed25519 signing key.
    pub fn open(arn: &str) -> Result<Self, SignerError> {
        let region = kms_key_region(arn)?.to_string();
        let output = aws_kms(
            &region,
            &[
                "get-public-key",
                "--key-id",
                arn,
                "--query",
                "[KeySpec,KeyUsage,PublicKey]",
            ],
        )?;
        let fields: Vec<&str> = output.split_whitespace().collect();
        let [key_spec, key_usage, public_key] = fields.as_slice() else {
            return Err(SignerError::new(
                "aws kms get-public-key returned an unexpected answer",
            ));
        };
        if *key_spec != "ECC_NIST_EDWARDS25519" || *key_usage != "SIGN_VERIFY" {
            return Err(SignerError::new(format!(
                "the KMS key is a {key_spec} {key_usage} key, not an Ed25519 signing key"
            )));
        }
        let der = STANDARD
            .decode(public_key)
            .map_err(|_| SignerError::new("the KMS public key is not base64"))?;
        Ok(Self {
            arn: arn.to_string(),
            region,
            verifying_key: ed25519_verifying_key(&der)?,
        })
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, SignerError> {
        let encoded_message = STANDARD.encode(message);
        let output = aws_kms(
            &self.region,
            &[
                "sign",
                "--key-id",
                &self.arn,
                "--message",
                &encoded_message,
                "--message-type",
                "RAW",
                "--signing-algorithm",
                "ED25519_SHA_512",
                "--query",
                "Signature",
            ],
        )?;
        let bytes = STANDARD
            .decode(output.trim())
            .map_err(|_| SignerError::new("the KMS signature is not base64"))?;
        let signature = Signature::from_slice(&bytes)
            .map_err(|_| SignerError::new("the KMS signature is not an Ed25519 signature"))?;
        self.verifying_key
            .verify(message, &signature)
            .map_err(|_| SignerError::new("the KMS signature does not verify with its key"))?;
        Ok(signature)
    }
}

/// The region of `arn:aws:kms:<region>:<account>:key/<id>`.
fn kms_key_region(arn: &str) -> Result<&str, SignerError> {
    let invalid =
        || SignerError::new("--kms-key must be a full KMS key ARN (arn:aws:kms:...:key/...)");
    let mut parts = arn.splitn(6, ':');
    let (Some("arn"), Some("aws"), Some("kms"), Some(region), Some(account), Some(resource)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(invalid());
    };
    let key_id = resource.strip_prefix("key/").ok_or_else(invalid)?;
    let valid_region = !region.is_empty()
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let valid_account = account.len() == 12 && account.bytes().all(|byte| byte.is_ascii_digit());
    let valid_key_id = !key_id.is_empty()
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if valid_region && valid_account && valid_key_id {
        Ok(region)
    } else {
        Err(invalid())
    }
}

fn ed25519_verifying_key(der: &[u8]) -> Result<VerifyingKey, SignerError> {
    let invalid = || SignerError::new("the KMS public key is not an Ed25519 public key");
    let key: [u8; 32] = der
        .strip_prefix(ED25519_PUBLIC_KEY_PREFIX.as_slice())
        .and_then(|key| key.try_into().ok())
        .ok_or_else(invalid)?;
    VerifyingKey::from_bytes(&key).map_err(|_| invalid())
}

/// Runs `aws kms <arguments>` and returns its text output.
fn aws_kms(region: &str, arguments: &[&str]) -> Result<String, SignerError> {
    let operation = arguments.first().copied().unwrap_or_default();
    let output = Command::new("aws")
        .arg("kms")
        .args(arguments)
        .args([
            "--region",
            region,
            "--output",
            "text",
            "--cli-binary-format",
            "base64",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|_| SignerError::new("the AWS CLI (aws) could not be run"))?;
    if !output.status.success() {
        let detail: String = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(MAXIMUM_REPORTED_CLI_ERROR_CHARS)
            .collect();
        return Err(SignerError::new(format!(
            "aws kms {operation} failed: {detail}"
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| SignerError::new(format!("aws kms {operation} returned non-UTF-8 output")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kms_keys_are_full_arns_with_their_region() {
        assert_eq!(
            kms_key_region(
                "arn:aws:kms:us-east-1:146727531914:key/508b70ef-f6b0-41a0-ae05-9457a34fbb7b"
            ),
            Ok("us-east-1")
        );
        for invalid in [
            "alias/aeris-release-signing",
            "508b70ef-f6b0-41a0-ae05-9457a34fbb7b",
            "arn:aws:kms:us-east-1:146727531914:alias/aeris-release-signing",
            "arn:aws:kms:us-east-1:1467:key/508b70ef",
            "arn:aws:s3:us-east-1:146727531914:key/508b70ef",
            "arn:aws:kms::146727531914:key/508b70ef",
            "arn:aws:kms:US-EAST-1:146727531914:key/508b70ef",
            "arn:aws:kms:us-east-1:146727531914:key/",
            "arn:aws:kms:us-east-1:146727531914:key/a b",
        ] {
            assert!(kms_key_region(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn kms_public_keys_decode_only_as_ed25519_subject_public_key_info() {
        let key = SigningKey::from_bytes(&[5; 32]).verifying_key();
        let mut der = ED25519_PUBLIC_KEY_PREFIX.to_vec();
        der.extend_from_slice(key.as_bytes());
        assert_eq!(ed25519_verifying_key(&der), Ok(key));
        assert!(ed25519_verifying_key(&der[..der.len() - 1]).is_err());
        let mut longer = der.clone();
        longer.push(0);
        assert!(ed25519_verifying_key(&longer).is_err());
        let mut other_algorithm = der;
        other_algorithm[8] = 0x71;
        assert!(ed25519_verifying_key(&other_algorithm).is_err());
    }

    #[test]
    fn key_file_signatures_verify_with_the_signer_key() {
        let signer = ReleaseSigner::KeyFile(SigningKey::from_bytes(&[6; 32]));
        let signature = signer.sign(b"message").expect("local keys sign");
        assert!(
            signer
                .verifying_key()
                .verify(b"message", &signature)
                .is_ok()
        );
    }
}
