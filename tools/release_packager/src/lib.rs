//! Release packaging. [`package_release`] lays out one signed release in an output tree shaped
//! like the release origin: the payload executables, the rollback-compatibility data, the
//! signed manifest and, against a predecessor, a signed block plan. [`write_channel_pointer`]
//! points the stable channel at a packaged release once its setup program sits beside it, and
//! [`inno_setup_arguments`] are the Inno Setup defines that build that setup program.
//!
//! Releases are signed by a [`ReleaseSigner`]: a local key file or an AWS KMS key. Nothing
//! here publishes. Uploading the output tree is a separate step the maintainer runs.

mod block_plan;
mod signer;

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use aeris_platform_runtime::fs::{StagedFile, write_file_durably};
use aeris_release_lifecycle::{
    BLOCK_PLAN_FILENAME, LAUNCHER_PROTOCOL_VERSION, RELEASE_CHANNEL,
    RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_FILENAME, RELEASE_MANIFEST_SCHEMA_VERSION,
    ROLLBACK_COMPATIBILITY_FILENAME, ReleaseChannelPointer, ReleaseFile, ReleaseFileRole,
    ReleaseInstallerMetadata, ReleaseManifest, ReleasePolicy, RollbackCompatibilityMetadata,
    RolloutMetadata, SignedReleaseManifest, UnsignedBlockPlan, UnsignedReleaseManifest,
    channel_pointer_path, installer_filename, release_directory_path, valid_release_identity,
    validate_release_channel, verify_block_plan, verify_release_file, verify_release_manifest,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

pub use signer::{KmsKey, ReleaseSigner, SignerError};

const MAXIMUM_SIGNING_KEY_FILE_BYTES: u64 = 256;
const MAXIMUM_METADATA_FILE_BYTES: u64 = 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// The desktop executable's file name in a payload and a release.
#[must_use]
pub fn desktop_file_name() -> String {
    format!("aeris_desktop{}", std::env::consts::EXE_SUFFIX)
}

/// The launcher executable's file name in a payload and a release.
#[must_use]
pub fn launcher_file_name() -> String {
    format!("aeris_launcher{}", std::env::consts::EXE_SUFFIX)
}

/// Reads the Ed25519 signing key from `path`: the unpadded base64url encoding of its 32-byte
/// seed, optionally followed by a line ending. The key comes only from a file, never from the
/// environment or the command line, and no error carries key material.
///
/// # Errors
/// Returns why the file is not a usable signing key.
pub fn read_signing_key(path: &Path) -> Result<SigningKey, SignerError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| SignerError::new("the signing key file is unavailable"))?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_SIGNING_KEY_FILE_BYTES {
        return Err(SignerError::new(
            "the signing key file is not a small regular file",
        ));
    }
    let text = Zeroizing::new(
        fs::read_to_string(path)
            .map_err(|_| SignerError::new("the signing key file is unreadable"))?,
    );
    let seed = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(text.trim_end_matches(['\r', '\n']))
            .map_err(|_| SignerError::new("the signing key is not unpadded base64url"))?,
    );
    let seed: Zeroizing<[u8; 32]> = Zeroizing::new(
        seed.as_slice()
            .try_into()
            .map_err(|_| SignerError::new("the signing key is not a 32-byte Ed25519 seed"))?,
    );
    Ok(SigningKey::from_bytes(&seed))
}

/// The verifying key a release build embeds as `AERIS_RELEASE_VERIFYING_KEY`.
#[must_use]
pub fn verifying_key_text(key: &VerifyingKey) -> String {
    URL_SAFE_NO_PAD.encode(key.as_bytes())
}

/// One release to package for this platform and architecture.
pub struct ReleaseRequest<'a> {
    /// Directory that holds exactly the desktop and launcher executables.
    pub payload: &'a Path,
    /// Root of the origin-shaped output tree.
    pub output: &'a Path,
    /// HTTPS release base URL, without a trailing slash.
    pub base_url: &'a str,
    pub release_identity: &'a str,
    pub install_generation: u64,
    pub rollout: RolloutMetadata,
    /// The newest trading store schema the payload's desktop opens; a launcher never rolls
    /// back to this release once the live store is newer.
    pub max_store_schema: u32,
    /// A release directory packaged earlier with the same key. Its files are the block plan's
    /// source, so launchers running that release download only the blocks that changed.
    pub previous: Option<&'a Path>,
}

/// One packaged release.
pub struct PackagedRelease {
    /// The release's directory in the output tree.
    pub directory: PathBuf,
    pub manifest: SignedReleaseManifest,
    pub block_plan: BlockPlanSummary,
}

/// What a packaged release's block plan covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockPlanSummary {
    /// No predecessor was given.
    NotRequested,
    /// No file shares a block with the predecessor; launchers download every file in full.
    NoSharedBlocks,
    /// `files` files are rebuilt from the predecessor plus `download_blocks` downloaded blocks.
    Written {
        files: usize,
        download_blocks: usize,
    },
}

/// Packages one release into its immutable directory below `request.output`. A failed
/// packaging removes the directory again, so no partial release is left behind.
///
/// # Errors
/// Returns why the release could not be packaged.
pub fn package_release(
    request: &ReleaseRequest<'_>,
    signer: &ReleaseSigner,
) -> Result<PackagedRelease, String> {
    validate_base_url(request.base_url)?;
    if !valid_release_identity(request.release_identity) {
        return Err(
            "the release identity must be 16 to 128 ASCII letters, digits, '_' or '-'".to_string(),
        );
    }
    if request.install_generation == 0 {
        return Err("the install generation must be at least 1".to_string());
    }
    if request.max_store_schema == 0 {
        return Err("the trading store schema must be at least 1".to_string());
    }
    let relative = release_directory_path(
        std::env::consts::OS,
        std::env::consts::ARCH,
        request.install_generation,
        request.release_identity,
    );
    let directory = native_path(request.output, &relative);
    if fs::symlink_metadata(&directory).is_ok() {
        return Err(format!(
            "{} already exists; a packaged release is never rewritten",
            directory.display()
        ));
    }
    fs::create_dir_all(&directory)
        .map_err(|_| format!("{} could not be created", directory.display()))?;
    let release_url = format!("{}/{relative}", request.base_url);
    match package_into(&directory, &release_url, request, signer) {
        Ok((manifest, block_plan)) => Ok(PackagedRelease {
            directory,
            manifest,
            block_plan,
        }),
        Err(error) => Err(match fs::remove_dir_all(&directory) {
            Ok(()) => error,
            Err(_) => format!(
                "{error}; the partial release {} could not be removed",
                directory.display()
            ),
        }),
    }
}

fn package_into(
    directory: &Path,
    release_url: &str,
    request: &ReleaseRequest<'_>,
    key: &ReleaseSigner,
) -> Result<(SignedReleaseManifest, BlockPlanSummary), String> {
    let desktop = desktop_file_name();
    let executables = [desktop.clone(), launcher_file_name()];
    check_payload(request.payload, &executables)?;
    let mut files = Vec::new();
    for name in &executables {
        let destination = directory.join(name);
        let (size, sha256) = copy_staged(&request.payload.join(name), &destination)?;
        files.push(ReleaseFile {
            role: if *name == desktop {
                ReleaseFileRole::Desktop
            } else {
                ReleaseFileRole::RuntimeAsset
            },
            path: name.clone(),
            url: format!("{release_url}/{name}"),
            size,
            sha256,
            executable: true,
        });
    }
    let compatibility = serde_json::to_vec(&RollbackCompatibilityMetadata::for_store_schema(
        request.max_store_schema,
    ))
    .map_err(|_| "the rollback-compatibility data could not be encoded".to_string())?;
    write_file_durably(
        &directory.join(ROLLBACK_COMPATIBILITY_FILENAME),
        &compatibility,
    )
    .map_err(|_| "the rollback-compatibility data could not be written".to_string())?;
    files.push(ReleaseFile {
        role: ReleaseFileRole::RuntimeAsset,
        path: ROLLBACK_COMPATIBILITY_FILENAME.to_string(),
        url: format!("{release_url}/{ROLLBACK_COMPATIBILITY_FILENAME}"),
        size: compatibility.len() as u64,
        sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(&compatibility)),
        executable: false,
    });
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let manifest = ReleaseManifest {
        schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
        release_identity: request.release_identity.to_string(),
        install_generation: request.install_generation,
        channel: RELEASE_CHANNEL.to_string(),
        minimum_launcher_protocol: LAUNCHER_PROTOCOL_VERSION,
        platform: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        files,
        rollout: request.rollout.clone(),
    };
    let unsigned = UnsignedReleaseManifest::new(manifest)
        .map_err(|error| format!("the release manifest was refused: {error}"))?;
    let manifest_signature = key
        .sign(unsigned.signing_message())
        .map_err(|error| format!("the release manifest could not be signed: {error}"))?;
    let signed = unsigned.with_signature(&manifest_signature);
    // Package only what this platform's launcher accepts.
    verify_release_manifest(
        &signed,
        &key.verifying_key(),
        &ReleasePolicy::native(request.install_generation),
    )
    .map_err(|error| format!("the signed release manifest does not verify: {error}"))?;
    write_json(&directory.join(RELEASE_MANIFEST_FILENAME), &signed)?;

    let block_plan = match request.previous {
        None => BlockPlanSummary::NotRequested,
        Some(previous) => write_block_plan(previous, directory, &signed, key)?,
    };
    Ok((signed, block_plan))
}

fn write_block_plan(
    previous: &Path,
    directory: &Path,
    target: &SignedReleaseManifest,
    key: &ReleaseSigner,
) -> Result<BlockPlanSummary, String> {
    let source: SignedReleaseManifest = read_json(&previous.join(RELEASE_MANIFEST_FILENAME))?;
    verify_release_manifest(&source, &key.verifying_key(), &ReleasePolicy::native(0))
        .map_err(|error| format!("the previous release does not verify with this key: {error}"))?;
    if source.manifest().install_generation >= target.manifest().install_generation {
        return Err("the previous release must have a lower install generation".to_string());
    }
    for file in &source.manifest().files {
        verify_release_file(&previous.join(&file.path), file).map_err(|error| {
            format!(
                "previous release file {} does not match: {error}",
                file.path
            )
        })?;
    }
    let Some(plan) = block_plan::plan(source.manifest(), previous, target.manifest(), directory)?
    else {
        return Ok(BlockPlanSummary::NoSharedBlocks);
    };
    let summary = BlockPlanSummary::Written {
        files: plan.files.len(),
        download_blocks: plan.files.iter().map(block_plan::download_blocks).sum(),
    };
    let unsigned = UnsignedBlockPlan::new(plan, source.manifest(), target.manifest())
        .map_err(|error| format!("the block plan was refused: {error}"))?;
    let plan_signature = key
        .sign(unsigned.signing_message())
        .map_err(|error| format!("the block plan could not be signed: {error}"))?;
    let signed = unsigned
        .with_signature(&plan_signature)
        .map_err(|error| format!("the block plan was refused: {error}"))?;
    verify_block_plan(
        &signed,
        source.manifest(),
        target.manifest(),
        &key.verifying_key(),
    )
    .map_err(|error| format!("the signed block plan does not verify: {error}"))?;
    write_json(&directory.join(BLOCK_PLAN_FILENAME), &signed)?;
    Ok(summary)
}

/// The stable channel pointer for one packaged release.
pub struct ChannelRequest<'a> {
    /// Root of the output tree the release was packaged into.
    pub output: &'a Path,
    /// HTTPS release base URL, without a trailing slash; the one the release was packaged for.
    pub base_url: &'a str,
    pub release_identity: &'a str,
    pub install_generation: u64,
    /// The product version the website shows for this release.
    pub version: &'a str,
    /// Publication time as `YYYY-MM-DDTHH:MM:SSZ`.
    pub published_at: &'a str,
}

/// Points this platform's stable channel at a packaged release whose setup program has been
/// built into the release directory. The pointer is checked exactly as a launcher checks it
/// before it is written.
///
/// # Errors
/// Returns why the pointer could not be built, validated or written.
pub fn write_channel_pointer(request: &ChannelRequest<'_>) -> Result<PathBuf, String> {
    validate_base_url(request.base_url)?;
    let relative = release_directory_path(
        std::env::consts::OS,
        std::env::consts::ARCH,
        request.install_generation,
        request.release_identity,
    );
    let directory = native_path(request.output, &relative);
    let signed: SignedReleaseManifest = read_json(&directory.join(RELEASE_MANIFEST_FILENAME))?;
    let installer = installer_filename();
    let installer_path = directory.join(&installer);
    let metadata = fs::symlink_metadata(&installer_path)
        .map_err(|_| format!("the setup program {} is missing", installer_path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!(
            "the setup program {} is not a non-empty regular file",
            installer_path.display()
        ));
    }
    let input = File::open(&installer_path)
        .map_err(|_| format!("{} could not be read", installer_path.display()))?;
    let (size, sha256) = hash_copy(input, &mut io::sink())
        .map_err(|_| format!("{} could not be read", installer_path.display()))?;
    let release_url = format!("{}/{relative}", request.base_url);
    let manifest = signed.manifest();
    let pointer = ReleaseChannelPointer {
        schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
        channel: manifest.channel.clone(),
        platform: manifest.platform.clone(),
        architecture: manifest.architecture.clone(),
        release_identity: manifest.release_identity.clone(),
        install_generation: manifest.install_generation,
        version: request.version.to_string(),
        published_at: request.published_at.to_string(),
        manifest_url: format!("{release_url}/{RELEASE_MANIFEST_FILENAME}"),
        installer: ReleaseInstallerMetadata {
            url: format!("{release_url}/{installer}"),
            filename: installer,
            size,
            sha256_b64url: sha256,
        },
        signed_release: signed.clone(),
    };
    validate_release_channel(&pointer, request.base_url)?;
    let path = native_path(
        request.output,
        &channel_pointer_path(std::env::consts::OS, std::env::consts::ARCH),
    );
    let parent = path
        .parent()
        .ok_or_else(|| "the channel pointer path is invalid".to_string())?;
    fs::create_dir_all(parent).map_err(|_| format!("{} could not be created", parent.display()))?;
    write_json(&path, &pointer)?;
    Ok(path)
}

/// The inputs of the Windows setup program for one packaged release.
pub struct InnoSetupRequest<'a> {
    /// `tools/windows/aeris_setup.iss`.
    pub script: &'a Path,
    pub icon: &'a Path,
    /// The product version the setup program shows.
    pub version: &'a str,
    /// The packaged release directory; the setup program is written into it.
    pub release_directory: &'a Path,
}

/// The ISCC arguments that build the setup program for a packaged release into its release
/// directory, where [`write_channel_pointer`] expects it. Pass absolute paths.
#[must_use]
pub fn inno_setup_arguments(request: &InnoSetupRequest<'_>) -> Vec<String> {
    let release_file = |name: &str| request.release_directory.join(name).display().to_string();
    vec![
        "/Q".to_string(),
        format!("/DAppVersion={}", request.version),
        format!("/DLauncherPath={}", release_file(&launcher_file_name())),
        format!("/DManifestPath={}", release_file(RELEASE_MANIFEST_FILENAME)),
        format!("/DDesktopPath={}", release_file(&desktop_file_name())),
        format!(
            "/DRollbackCompatibilityPath={}",
            release_file(ROLLBACK_COMPATIBILITY_FILENAME)
        ),
        format!("/DIconPath={}", request.icon.display()),
        format!("/DOutputDir={}", request.release_directory.display()),
        request.script.display().to_string(),
    ]
}

fn validate_base_url(base_url: &str) -> Result<(), String> {
    let host = base_url.strip_prefix("https://").unwrap_or_default();
    if host.is_empty()
        || host.starts_with('/')
        || base_url.ends_with('/')
        || base_url
            .contains(|character: char| character.is_whitespace() || "?#".contains(character))
    {
        return Err("the base URL must be an https URL without a trailing slash".to_string());
    }
    Ok(())
}

/// Every payload entry is one of `executables` and every executable is present.
fn check_payload(payload: &Path, executables: &[String]) -> Result<(), String> {
    let entries =
        fs::read_dir(payload).map_err(|_| format!("{} could not be listed", payload.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| format!("{} could not be listed", payload.display()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "the payload holds a file whose name is not Unicode".to_string())?;
        if !executables.contains(&name) {
            return Err(format!(
                "the payload holds {name}, which a release does not carry"
            ));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| format!("payload file {name} is unavailable"))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(format!(
                "payload file {name} is not a non-empty regular file"
            ));
        }
        found.push(name);
    }
    match executables.iter().find(|name| !found.contains(name)) {
        Some(missing) => Err(format!("the payload has no {missing}")),
        None => Ok(()),
    }
}

/// Copies `source` to `destination` through a staged file and returns its size and SHA-256.
fn copy_staged(source: &Path, destination: &Path) -> Result<(u64, String), String> {
    let failed = || format!("{} could not be copied", source.display());
    let input = File::open(source).map_err(|_| failed())?;
    let mut staged = StagedFile::create(destination).map_err(|_| failed())?;
    let digest = hash_copy(input, &mut staged).map_err(|_| failed())?;
    staged.commit().map_err(|_| failed())?;
    Ok(digest)
}

fn hash_copy(mut input: impl Read, output: &mut impl Write) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
        size += read as u64;
    }
    Ok((size, URL_SAFE_NO_PAD.encode(hasher.finalize())))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| format!("{} is missing", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_METADATA_FILE_BYTES {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let bytes = fs::read(path).map_err(|_| format!("{} could not be read", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|_| format!("{} is malformed", path.display()))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| format!("{} could not be encoded", path.display()))?;
    bytes.push(b'\n');
    write_file_durably(path, &bytes).map_err(|_| format!("{} could not be written", path.display()))
}

/// `root` joined with a `/`-separated origin path.
fn native_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_keys_load_only_from_an_exact_seed_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let directory = std::env::temp_dir().join(format!("aeris-packager-key-{nanos}"));
        fs::create_dir_all(&directory).expect("key directory");
        let path = directory.join("release.key");
        let seed = [9_u8; 32];
        fs::write(&path, format!("{}\n", URL_SAFE_NO_PAD.encode(seed))).expect("key file");
        let key = read_signing_key(&path).expect("key loads");
        assert_eq!(key.to_bytes(), seed);
        assert_eq!(
            verifying_key_text(&key.verifying_key()),
            URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&seed).verifying_key().as_bytes())
        );
        for invalid in [
            URL_SAFE_NO_PAD.encode([9_u8; 31]),
            format!("{}=", URL_SAFE_NO_PAD.encode(seed)),
            "not a key".to_string(),
        ] {
            fs::write(&path, invalid).expect("key file");
            let Err(error) = read_signing_key(&path) else {
                panic!("invalid key accepted");
            };
            assert!(!error.to_string().contains(&URL_SAFE_NO_PAD.encode(seed)));
        }
        fs::remove_dir_all(directory).expect("remove key directory");
    }

    #[test]
    fn base_urls_must_be_https_without_a_trailing_slash() {
        assert!(validate_base_url("https://downloads.example.test/aeris").is_ok());
        for invalid in [
            "http://downloads.example.test",
            "https://",
            "https:///aeris",
            "https://downloads.example.test/",
            "https://downloads.example.test/a b",
            "https://downloads.example.test/?channel",
        ] {
            assert!(validate_base_url(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn inno_setup_arguments_name_every_define_the_setup_script_requires() {
        let arguments = inno_setup_arguments(&InnoSetupRequest {
            script: Path::new("aeris_setup.iss"),
            icon: Path::new("aeris.ico"),
            version: "1.2.0",
            release_directory: Path::new("release"),
        });
        let script = include_str!("../../windows/aeris_setup.iss");
        for define in [
            "AppVersion",
            "LauncherPath",
            "ManifestPath",
            "DesktopPath",
            "RollbackCompatibilityPath",
            "IconPath",
            "OutputDir",
        ] {
            assert!(script.contains(&format!("#ifndef {define}")), "{define}");
            assert!(
                arguments
                    .iter()
                    .any(|argument| argument.starts_with(&format!("/D{define}="))),
                "{define}"
            );
        }
        // The channel pointer names the setup program ISCC writes into the release directory.
        assert!(
            script
                .lines()
                .any(|line| line.trim() == "OutputBaseFilename=Aeris-Setup")
        );
        assert_eq!(
            arguments.last().map(String::as_str),
            Some("aeris_setup.iss")
        );
    }

    #[test]
    fn setup_script_installs_while_it_can_fail_and_confirms_uninstall_once() {
        let script = include_str!("../../windows/aeris_setup.iss");
        let files = script
            .split("[Files]")
            .nth(1)
            .and_then(|section| section.split("\n[").next())
            .expect("the setup script has a [Files] section");
        let last_file = files
            .lines()
            .rfind(|line| line.starts_with("Source:"))
            .expect("the [Files] section names files");
        // An ssPostInstall exception is only displayed and Setup exits 0, so the
        // verified install must run inside the file copy, where a failure rolls back.
        assert!(last_file.ends_with("AfterInstall: InstallBundledRelease"));
        assert!(script.contains("procedure InstallBundledRelease;"));
        // Inno's own confirmation box is skipped only for a silent uninstall.
        assert!(script.contains("'\" /SILENT ' + InteractiveRemovalSwitch"));
        assert!(script.contains("InteractiveRemovalSwitch = '/INTERACTIVE';"));
        // Removing the 1.x desktop before the install succeeds would strand a failed upgrade.
        let install_delete = script
            .split("[InstallDelete]")
            .nth(1)
            .and_then(|section| section.split("\n[").next())
            .expect("the setup script has an [InstallDelete] section");
        assert!(
            !install_delete
                .lines()
                .any(|line| line.starts_with("Type:") && line.contains("aeris_desktop.exe"))
        );
    }
}
