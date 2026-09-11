//! Signed, bounded metadata for bandwidth-efficient release reconstruction.
//!
//! Block plans are an optional optimization layered beside the canonical
//! release manifest. The release manifest remains the authority for the final
//! file size and SHA-256; a block plan only describes how a launcher may
//! reconstruct some target files from one already-verified predecessor plus
//! bounded byte ranges from the target artifact.

use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    path::{Component, Path},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::ReleaseManifest;

/// Wire schema for independently signed release block plans.
pub const BLOCK_PLAN_SCHEMA_VERSION: u32 = 1;
/// Immutable sidecar filename used beside one canonical release manifest.
pub const BLOCK_PLAN_FILENAME: &str = "block-plan.json";
/// Fixed target/source block size used by block plans.
pub const BLOCK_PLAN_BLOCK_BYTES: u64 = 1024 * 1024;
/// Maximum encoded signed sidecar size accepted by the release system.
pub const MAXIMUM_SIGNED_BLOCK_PLAN_BYTES: usize = 1024 * 1024;
/// Maximum number of target files described by one plan.
pub const MAXIMUM_BLOCK_PLAN_FILES: usize = 256;
/// Maximum number of target blocks described by one plan.
pub const MAXIMUM_BLOCK_PLAN_BLOCKS: usize = 4096;
/// Maximum number of network range requests a valid plan may require.
pub const MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS: usize = 256;

const BLOCK_PLAN_SIGNING_DOMAIN: &[u8] = b"AXIUSFLOW_RELEASE_BLOCK_PLAN_V1\0";

/// One independently signed reconstruction plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedBlockPlan {
    pub plan: BlockPlan,
    pub signature: String,
}

/// Immutable binding between one verified source release and a newer target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockPlan {
    pub schema_version: u32,
    pub source_release_identity: String,
    pub source_install_generation: u64,
    pub target_release_identity: String,
    pub target_install_generation: u64,
    pub files: Vec<BlockFilePlan>,
}

/// Reconstruction metadata for one same-path source/target release file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockFilePlan {
    pub path: String,
    pub source_size: u64,
    pub source_sha256: String,
    pub target_size: u64,
    pub target_sha256: String,
    pub blocks: Vec<BlockDescriptor>,
}

/// One fixed-position target block.
///
/// `source_offset = Some(offset)` means the bytes may be copied from the
/// verified predecessor at that aligned offset. `None` means the launcher must
/// fetch the target block from the canonical target artifact at the block's
/// implicit cumulative target offset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockDescriptor {
    pub length: u32,
    pub sha256: String,
    pub source_offset: Option<u64>,
}

/// Fail-closed block-plan validation category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseDeliveryError {
    InvalidPlan,
    InvalidSignature,
    BindingMismatch,
}

impl fmt::Display for ReleaseDeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPlan => "release block plan is invalid",
            Self::InvalidSignature => "release block plan signature verification failed",
            Self::BindingMismatch => {
                "release block plan does not match its source and target releases"
            }
        })
    }
}

impl Error for ReleaseDeliveryError {}

/// Validates source/target binding and signs one bounded block plan.
///
/// The source and target manifests are expected to have already crossed the
/// normal release-manifest trust boundary. Requiring them here prevents a
/// publisher from signing a plan whose duplicated identity, size, or file hash
/// metadata does not exactly match those manifests.
///
/// # Errors
/// Returns an error for malformed/bounds-violating plans, source/target
/// mismatches, or an encoded signed envelope that exceeds the metadata bound.
pub fn sign_block_plan(
    plan: BlockPlan,
    source: &ReleaseManifest,
    target: &ReleaseManifest,
    key: &SigningKey,
) -> Result<SignedBlockPlan, ReleaseDeliveryError> {
    validate_block_plan_binding(&plan, source, target)?;
    let canonical = canonical_block_plan(&plan)?;
    let signed = SignedBlockPlan {
        plan,
        signature: URL_SAFE_NO_PAD.encode(key.sign(&canonical).to_bytes()),
    };
    validate_signed_size(&signed)?;
    Ok(signed)
}

/// Verifies the plan signature, bounds, and exact source/target manifest binding.
///
/// # Errors
/// Rejects malformed metadata, invalid signatures, oversized sidecars, or any
/// mismatch with the authenticated predecessor/target release manifests.
pub fn verify_block_plan(
    signed: &SignedBlockPlan,
    source: &ReleaseManifest,
    target: &ReleaseManifest,
    key: &VerifyingKey,
) -> Result<(), ReleaseDeliveryError> {
    validate_plan_shape(&signed.plan)?;
    validate_signed_size(signed)?;
    let canonical = canonical_block_plan(&signed.plan)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| ReleaseDeliveryError::InvalidSignature)?;
    let signature =
        Signature::from_slice(&bytes).map_err(|_| ReleaseDeliveryError::InvalidSignature)?;
    key.verify(&canonical, &signature)
        .map_err(|_| ReleaseDeliveryError::InvalidSignature)?;
    validate_manifest_binding(&signed.plan, source, target)
}

/// Decodes bounded sidecar bytes, then verifies signature and manifest binding.
///
/// The encoded-size check happens before JSON deserialization so untrusted
/// transport bytes cannot bypass the sidecar allocation bound.
///
/// # Errors
/// Rejects empty, oversized, malformed, unsigned, or incorrectly bound input.
pub fn decode_and_verify_block_plan(
    bytes: &[u8],
    source: &ReleaseManifest,
    target: &ReleaseManifest,
    key: &VerifyingKey,
) -> Result<SignedBlockPlan, ReleaseDeliveryError> {
    if bytes.is_empty() || bytes.len() > MAXIMUM_SIGNED_BLOCK_PLAN_BYTES {
        return Err(ReleaseDeliveryError::InvalidPlan);
    }
    let signed: SignedBlockPlan =
        serde_json::from_slice(bytes).map_err(|_| ReleaseDeliveryError::InvalidPlan)?;
    verify_block_plan(&signed, source, target, key)?;
    Ok(signed)
}

/// Validates plan shape plus exact binding to source and target manifests.
///
/// This is useful while constructing a plan before signing it.
///
/// # Errors
/// Rejects malformed/bounds-violating metadata or any duplicated manifest field
/// that does not exactly match the supplied source/target release.
pub fn validate_block_plan_binding(
    plan: &BlockPlan,
    source: &ReleaseManifest,
    target: &ReleaseManifest,
) -> Result<(), ReleaseDeliveryError> {
    validate_plan_shape(plan)?;
    validate_manifest_binding(plan, source, target)
}

fn validate_plan_shape(plan: &BlockPlan) -> Result<(), ReleaseDeliveryError> {
    if plan.schema_version != BLOCK_PLAN_SCHEMA_VERSION
        || plan.source_install_generation == 0
        || plan.target_install_generation == 0
        || plan.source_install_generation >= plan.target_install_generation
        || !valid_identifier(&plan.source_release_identity, 128)
        || !valid_identifier(&plan.target_release_identity, 128)
        || plan.files.is_empty()
        || plan.files.len() > MAXIMUM_BLOCK_PLAN_FILES
    {
        return Err(ReleaseDeliveryError::InvalidPlan);
    }

    let mut paths = BTreeSet::new();
    let mut previous_path = None;
    let mut block_count = 0_usize;
    let mut download_count = 0_usize;
    for file in &plan.files {
        if file.source_size == 0
            || file.target_size == 0
            || !safe_relative_path(Path::new(&file.path))
            || !valid_digest(&file.source_sha256)
            || !valid_digest(&file.target_sha256)
            || file.blocks.is_empty()
            || !paths.insert(file.path.as_str())
            || previous_path.is_some_and(|path| path >= file.path.as_str())
        {
            return Err(ReleaseDeliveryError::InvalidPlan);
        }
        previous_path = Some(file.path.as_str());
        block_count = block_count
            .checked_add(file.blocks.len())
            .ok_or(ReleaseDeliveryError::InvalidPlan)?;
        if block_count > MAXIMUM_BLOCK_PLAN_BLOCKS {
            return Err(ReleaseDeliveryError::InvalidPlan);
        }

        let mut target_offset = 0_u64;
        let mut has_source_block = false;
        for block in &file.blocks {
            let remaining = file
                .target_size
                .checked_sub(target_offset)
                .ok_or(ReleaseDeliveryError::InvalidPlan)?;
            let expected_length = remaining.min(BLOCK_PLAN_BLOCK_BYTES);
            if expected_length == 0
                || u64::from(block.length) != expected_length
                || !valid_digest(&block.sha256)
            {
                return Err(ReleaseDeliveryError::InvalidPlan);
            }
            if let Some(source_offset) = block.source_offset {
                has_source_block = true;
                let source_end = source_offset
                    .checked_add(u64::from(block.length))
                    .ok_or(ReleaseDeliveryError::InvalidPlan)?;
                if source_offset % BLOCK_PLAN_BLOCK_BYTES != 0 || source_end > file.source_size {
                    return Err(ReleaseDeliveryError::InvalidPlan);
                }
            } else {
                download_count = download_count
                    .checked_add(1)
                    .ok_or(ReleaseDeliveryError::InvalidPlan)?;
                if download_count > MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS {
                    return Err(ReleaseDeliveryError::InvalidPlan);
                }
            }
            target_offset = target_offset
                .checked_add(u64::from(block.length))
                .ok_or(ReleaseDeliveryError::InvalidPlan)?;
        }
        if target_offset != file.target_size || !has_source_block {
            return Err(ReleaseDeliveryError::InvalidPlan);
        }
    }
    canonical_block_plan(plan).map(|_| ())
}

fn validate_manifest_binding(
    plan: &BlockPlan,
    source: &ReleaseManifest,
    target: &ReleaseManifest,
) -> Result<(), ReleaseDeliveryError> {
    if plan.source_release_identity != source.release_identity
        || plan.source_install_generation != source.install_generation
        || plan.target_release_identity != target.release_identity
        || plan.target_install_generation != target.install_generation
        || source.install_generation >= target.install_generation
    {
        return Err(ReleaseDeliveryError::BindingMismatch);
    }
    for planned in &plan.files {
        let source_file = source
            .files
            .iter()
            .find(|file| file.path == planned.path)
            .ok_or(ReleaseDeliveryError::BindingMismatch)?;
        let target_file = target
            .files
            .iter()
            .find(|file| file.path == planned.path)
            .ok_or(ReleaseDeliveryError::BindingMismatch)?;
        if planned.source_size != source_file.size
            || planned.source_sha256 != source_file.sha256
            || planned.target_size != target_file.size
            || planned.target_sha256 != target_file.sha256
        {
            return Err(ReleaseDeliveryError::BindingMismatch);
        }
    }
    Ok(())
}

fn canonical_block_plan(plan: &BlockPlan) -> Result<Vec<u8>, ReleaseDeliveryError> {
    let encoded = serde_json::to_vec(plan).map_err(|_| ReleaseDeliveryError::InvalidPlan)?;
    let capacity = BLOCK_PLAN_SIGNING_DOMAIN
        .len()
        .checked_add(encoded.len())
        .ok_or(ReleaseDeliveryError::InvalidPlan)?;
    if capacity > MAXIMUM_SIGNED_BLOCK_PLAN_BYTES {
        return Err(ReleaseDeliveryError::InvalidPlan);
    }
    let mut canonical = Vec::with_capacity(capacity);
    canonical.extend_from_slice(BLOCK_PLAN_SIGNING_DOMAIN);
    canonical.extend_from_slice(&encoded);
    Ok(canonical)
}

fn validate_signed_size(signed: &SignedBlockPlan) -> Result<(), ReleaseDeliveryError> {
    let encoded = serde_json::to_vec(signed).map_err(|_| ReleaseDeliveryError::InvalidPlan)?;
    if encoded.len() > MAXIMUM_SIGNED_BLOCK_PLAN_BYTES {
        return Err(ReleaseDeliveryError::InvalidPlan);
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|digest| digest.len() == 32)
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(value) if !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RELEASE_MANIFEST_SCHEMA_VERSION, ReleaseFile, ReleaseFileRole, RolloutMetadata};
    use sha2::{Digest as _, Sha256};

    fn digest(byte: u8) -> String {
        URL_SAFE_NO_PAD.encode([byte; 32])
    }

    fn manifest(identity: &str, generation: u64, size: u64, digest_byte: u8) -> ReleaseManifest {
        ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: identity.to_string(),
            install_generation: generation,
            channel: "stable".to_string(),
            minimum_version: "0.2.0".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            files: vec![ReleaseFile {
                role: ReleaseFileRole::Desktop,
                path: "axiusflow_desktop".to_string(),
                url: format!("https://releases.axiusflow.test/{generation}/axiusflow_desktop"),
                size,
                sha256: digest(digest_byte),
                executable: true,
            }],
            rollout: RolloutMetadata {
                cohort: "all".to_string(),
                percentage: 100,
            },
        }
    }

    fn plan(source: &ReleaseManifest, target: &ReleaseManifest) -> BlockPlan {
        let source_file = &source.files[0];
        let target_file = &target.files[0];
        BlockPlan {
            schema_version: BLOCK_PLAN_SCHEMA_VERSION,
            source_release_identity: source.release_identity.clone(),
            source_install_generation: source.install_generation,
            target_release_identity: target.release_identity.clone(),
            target_install_generation: target.install_generation,
            files: vec![BlockFilePlan {
                path: target_file.path.clone(),
                source_size: source_file.size,
                source_sha256: source_file.sha256.clone(),
                target_size: target_file.size,
                target_sha256: target_file.sha256.clone(),
                blocks: vec![
                    BlockDescriptor {
                        length: u32::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits u32"),
                        sha256: digest(11),
                        source_offset: Some(0),
                    },
                    BlockDescriptor {
                        length: 3,
                        sha256: digest(12),
                        source_offset: None,
                    },
                ],
            }],
        }
    }

    #[test]
    fn signed_block_plan_verifies_and_binds_both_release_manifests() {
        let size = BLOCK_PLAN_BLOCK_BYTES + 3;
        let source = manifest("source-release", 7, size, 1);
        let target = manifest("target-release", 8, size, 2);
        let key = SigningKey::from_bytes(&[7; 32]);
        let signed = sign_block_plan(plan(&source, &target), &source, &target, &key)
            .expect("valid plan signs");

        verify_block_plan(&signed, &source, &target, &key.verifying_key())
            .expect("signed plan verifies");
        assert!(
            serde_json::to_vec(&signed).expect("wire encoding").len()
                <= MAXIMUM_SIGNED_BLOCK_PLAN_BYTES
        );
    }

    #[test]
    fn signature_and_manifest_binding_tampering_fail_closed() {
        let size = BLOCK_PLAN_BLOCK_BYTES + 3;
        let source = manifest("source-release", 7, size, 1);
        let target = manifest("target-release", 8, size, 2);
        let key = SigningKey::from_bytes(&[8; 32]);
        let mut signed = sign_block_plan(plan(&source, &target), &source, &target, &key)
            .expect("valid plan signs");
        signed.plan.files[0].blocks[0].sha256 = digest(99);
        assert_eq!(
            verify_block_plan(&signed, &source, &target, &key.verifying_key()),
            Err(ReleaseDeliveryError::InvalidSignature)
        );

        let signed = sign_block_plan(plan(&source, &target), &source, &target, &key)
            .expect("valid plan signs");
        let mut different_target = target.clone();
        different_target.files[0].sha256 = digest(77);
        assert_eq!(
            verify_block_plan(&signed, &source, &different_target, &key.verifying_key()),
            Err(ReleaseDeliveryError::BindingMismatch)
        );
    }

    #[test]
    fn malformed_block_lengths_hashes_and_source_offsets_are_rejected() {
        let size = BLOCK_PLAN_BLOCK_BYTES + 3;
        let source = manifest("source-release", 7, size, 1);
        let target = manifest("target-release", 8, size, 2);

        let mut malformed = plan(&source, &target);
        malformed.files[0].blocks[0].length -= 1;
        assert_eq!(
            validate_block_plan_binding(&malformed, &source, &target),
            Err(ReleaseDeliveryError::InvalidPlan)
        );

        let mut malformed = plan(&source, &target);
        malformed.files[0].blocks[0].sha256 = URL_SAFE_NO_PAD.encode([1_u8; 31]);
        assert_eq!(
            validate_block_plan_binding(&malformed, &source, &target),
            Err(ReleaseDeliveryError::InvalidPlan)
        );

        let mut malformed = plan(&source, &target);
        malformed.files[0].blocks[0].source_offset = Some(1);
        assert_eq!(
            validate_block_plan_binding(&malformed, &source, &target),
            Err(ReleaseDeliveryError::InvalidPlan)
        );
    }

    #[test]
    fn total_block_and_download_request_bounds_are_enforced() {
        let source_size = BLOCK_PLAN_BLOCK_BYTES * (MAXIMUM_BLOCK_PLAN_BLOCKS as u64 + 1);
        let source = manifest("source-release", 7, source_size, 1);
        let target = manifest("target-release", 8, source_size, 2);
        let source_file = &source.files[0];
        let target_file = &target.files[0];
        let blocks = (0..=MAXIMUM_BLOCK_PLAN_BLOCKS)
            .map(|index| BlockDescriptor {
                length: u32::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits u32"),
                sha256: digest(3),
                source_offset: Some(index as u64 * BLOCK_PLAN_BLOCK_BYTES),
            })
            .collect();
        let too_many_blocks = BlockPlan {
            schema_version: BLOCK_PLAN_SCHEMA_VERSION,
            source_release_identity: source.release_identity.clone(),
            source_install_generation: source.install_generation,
            target_release_identity: target.release_identity.clone(),
            target_install_generation: target.install_generation,
            files: vec![BlockFilePlan {
                path: target_file.path.clone(),
                source_size: source_file.size,
                source_sha256: source_file.sha256.clone(),
                target_size: target_file.size,
                target_sha256: target_file.sha256.clone(),
                blocks,
            }],
        };
        assert_eq!(
            validate_block_plan_binding(&too_many_blocks, &source, &target),
            Err(ReleaseDeliveryError::InvalidPlan)
        );

        let blocks = MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS + 2;
        let size = BLOCK_PLAN_BLOCK_BYTES * blocks as u64;
        let source = manifest("source-release", 9, size, 4);
        let target = manifest("target-release", 10, size, 5);
        let mut too_many_downloads = BlockPlan {
            schema_version: BLOCK_PLAN_SCHEMA_VERSION,
            source_release_identity: source.release_identity.clone(),
            source_install_generation: source.install_generation,
            target_release_identity: target.release_identity.clone(),
            target_install_generation: target.install_generation,
            files: vec![BlockFilePlan {
                path: target.files[0].path.clone(),
                source_size: source.files[0].size,
                source_sha256: source.files[0].sha256.clone(),
                target_size: target.files[0].size,
                target_sha256: target.files[0].sha256.clone(),
                blocks: (0..blocks)
                    .map(|_| BlockDescriptor {
                        length: u32::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits u32"),
                        sha256: digest(6),
                        source_offset: None,
                    })
                    .collect(),
            }],
        };
        too_many_downloads.files[0].blocks[0].source_offset = Some(0);
        assert_eq!(
            validate_block_plan_binding(&too_many_downloads, &source, &target),
            Err(ReleaseDeliveryError::InvalidPlan)
        );
    }

    #[test]
    fn signing_domain_is_distinct_from_plain_plan_json() {
        let size = BLOCK_PLAN_BLOCK_BYTES + 3;
        let source = manifest("source-release", 7, size, 1);
        let target = manifest("target-release", 8, size, 2);
        let key = SigningKey::from_bytes(&[9; 32]);
        let plan = plan(&source, &target);
        let signed = sign_block_plan(plan.clone(), &source, &target, &key).expect("plan signs");
        let plain_json_signature = key.sign(&serde_json::to_vec(&plan).expect("plan JSON"));
        assert_ne!(
            URL_SAFE_NO_PAD.encode(plain_json_signature.to_bytes()),
            signed.signature
        );
        assert_ne!(
            Sha256::digest(BLOCK_PLAN_SIGNING_DOMAIN),
            Sha256::digest(b"release-manifest")
        );
    }

    #[test]
    fn encoded_sidecar_is_bounded_before_decode_and_verification() {
        let size = BLOCK_PLAN_BLOCK_BYTES + 3;
        let source = manifest("source-release", 7, size, 1);
        let target = manifest("target-release", 8, size, 2);
        let key = SigningKey::from_bytes(&[10; 32]);
        let signed = sign_block_plan(plan(&source, &target), &source, &target, &key)
            .expect("valid plan signs");
        let encoded = serde_json::to_vec(&signed).expect("sidecar encodes");
        let decoded =
            decode_and_verify_block_plan(&encoded, &source, &target, &key.verifying_key())
                .expect("bounded sidecar verifies");
        assert_eq!(decoded, signed);

        let oversized = vec![b' '; MAXIMUM_SIGNED_BLOCK_PLAN_BYTES + 1];
        assert_eq!(
            decode_and_verify_block_plan(&oversized, &source, &target, &key.verifying_key(),),
            Err(ReleaseDeliveryError::InvalidPlan)
        );
    }
}
