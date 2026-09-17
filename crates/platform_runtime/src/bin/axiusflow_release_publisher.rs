//! Local, offline-key release packager and optional Wrangler R2 publisher.
//!
//! The signing key is read only by this process from an explicit local file.
//! Child Cargo/Wrangler processes receive the derived public key, never the
//! private key bytes or path through an environment variable.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use axiusflow_platform_runtime::{
    BLOCK_PLAN_BLOCK_BYTES, BLOCK_PLAN_FILENAME, BLOCK_PLAN_SCHEMA_VERSION, BlockDescriptor,
    BlockFilePlan, BlockPlan, MAXIMUM_BLOCK_PLAN_BLOCKS, MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS,
    RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_SCHEMA_VERSION,
    ROLLBACK_COMPATIBILITY_FILENAME, ReleaseChannelPointer, ReleaseFile, ReleaseFileRole,
    ReleaseInstallerMetadata, ReleaseManifest, ReleasePolicy, RollbackCompatibilityMetadata,
    RolloutMetadata, SignedReleaseManifest, decode_and_verify_block_plan, replace_file_atomically,
    sign_block_plan, sign_release_manifest, verify_release_file, verify_release_manifest,
    verify_release_manifest_signature,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const DEFAULT_CHANNEL: &str = "stable";
const DEFAULT_OUTPUT_ROOT: &str = "target/release-publish";
const WRANGLER_MAXIMUM_OBJECT_BYTES: u64 = 315 * 1024 * 1024;
const MAXIMUM_CHANNEL_BYTES: u64 = 1024 * 1024;
const RELEASE_PROVENANCE_SCHEMA_VERSION: u32 = 1;
const RELEASE_PROVENANCE_SIGNATURE_DOMAIN: &[u8] = b"AXIUSFLOW_RELEASE_PROVENANCE_V1\0";
const RELEASE_RETIREMENT_SCHEMA_VERSION: u32 = 1;
const RELEASE_RETIREMENT_FILENAME: &str = "retirement.json";
const RELEASE_RETIREMENT_SIGNATURE_DOMAIN: &[u8] = b"AXIUSFLOW_RELEASE_RETIREMENT_V1\0";
const MAXIMUM_RETIREMENT_BYTES: u64 = 2 * 1024 * 1024;
const PUBLIC_VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);
const VERIFY_AUTHENTICODE_METADATA: &str = r"$signature = Get-AuthenticodeSignature -LiteralPath $env:AXIUSFLOW_AUTHENTICODE_PATH; if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ine $env:AXIUSFLOW_AUTHENTICODE_CERT_SHA1 -or $null -eq $signature.TimeStamperCertificate) { exit 1 }";

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("Axiusflow release publisher: {error}");
        std::process::exit(1);
    }
}

#[derive(Debug)]
struct PublisherConfig {
    signing_key_file: PathBuf,
    expected_verifying_key: Option<VerifyingKey>,
    skip_build: bool,
    release_identity: String,
    generation: u64,
    release_version: String,
    minimum_version: String,
    base_url: String,
    published_at: String,
    channel: String,
    rollout_cohort: String,
    rollout_percentage: u8,
    output_root: PathBuf,
    r2_bucket: Option<String>,
    wrangler: OsString,
    iscc: OsString,
    authenticode_tool: OsString,
    authenticode_certificate_sha1: Option<String>,
    authenticode_timestamp_url: Option<String>,
}

impl PublisherConfig {
    #[allow(clippy::too_many_lines)]
    fn parse(arguments: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut signing_key_file = None;
        let mut expected_verifying_key = None;
        let mut skip_build = false;
        let mut release_identity = None;
        let mut generation = None;
        let mut release_version = None;
        let mut minimum_version = None;
        let mut base_url = None;
        let mut published_at = None;
        let mut channel = DEFAULT_CHANNEL.to_string();
        let mut rollout_cohort = "all".to_string();
        let mut rollout_percentage = 100_u8;
        let mut output_root = PathBuf::from(DEFAULT_OUTPUT_ROOT);
        let mut r2_bucket = None;
        let mut wrangler = OsString::from("wrangler");
        let mut iscc = OsString::from("ISCC.exe");
        let mut authenticode_tool = OsString::from("signtool.exe");
        let mut authenticode_certificate_sha1 = None;
        let mut authenticode_timestamp_url = None;
        let mut arguments = arguments;
        while let Some(flag) = arguments.next() {
            let flag = flag
                .into_string()
                .map_err(|_| "release publisher argument is not valid UTF-8".to_string())?;
            match flag.as_str() {
                "--signing-key-file" => {
                    signing_key_file =
                        Some(PathBuf::from(required_argument(&mut arguments, &flag)?));
                }
                "--expected-verifying-key" => {
                    let encoded = required_argument(&mut arguments, &flag)?;
                    expected_verifying_key = Some(parse_verifying_key(&encoded)?);
                }
                "--skip-build" => skip_build = true,
                "--release-identity" => {
                    release_identity = Some(required_argument(&mut arguments, &flag)?);
                }
                "--generation" => {
                    let raw = required_argument(&mut arguments, &flag)?;
                    generation = Some(
                        raw.parse::<u64>()
                            .ok()
                            .filter(|value| *value > 0)
                            .ok_or_else(|| {
                                "release generation must be a positive integer".to_string()
                            })?,
                    );
                }
                "--release-version" => {
                    release_version = Some(required_argument(&mut arguments, &flag)?);
                }
                "--minimum-version" => {
                    minimum_version = Some(required_argument(&mut arguments, &flag)?);
                }
                "--base-url" => base_url = Some(required_argument(&mut arguments, &flag)?),
                "--published-at" => published_at = Some(required_argument(&mut arguments, &flag)?),
                "--channel" => channel = required_argument(&mut arguments, &flag)?,
                "--rollout-cohort" => {
                    rollout_cohort = required_argument(&mut arguments, &flag)?;
                }
                "--rollout-percentage" => {
                    let raw = required_argument(&mut arguments, &flag)?;
                    rollout_percentage = raw
                        .parse::<u8>()
                        .ok()
                        .filter(|value| *value <= 100)
                        .ok_or_else(|| {
                            "release rollout percentage must be an integer from 0 through 100"
                                .to_string()
                        })?;
                }
                "--output" => {
                    output_root = PathBuf::from(required_argument(&mut arguments, &flag)?);
                }
                "--r2-bucket" => r2_bucket = Some(required_argument(&mut arguments, &flag)?),
                "--wrangler" => {
                    wrangler = OsString::from(required_argument(&mut arguments, &flag)?);
                }
                "--iscc" => {
                    iscc = OsString::from(required_argument(&mut arguments, &flag)?);
                }
                "--authenticode-tool" => {
                    authenticode_tool = OsString::from(required_argument(&mut arguments, &flag)?);
                }
                "--authenticode-certificate-sha1" => {
                    authenticode_certificate_sha1 = Some(required_argument(&mut arguments, &flag)?);
                }
                "--authenticode-timestamp-url" => {
                    authenticode_timestamp_url = Some(required_argument(&mut arguments, &flag)?);
                }
                _ => return Err(usage()),
            }
        }
        let config = Self {
            signing_key_file: signing_key_file.ok_or_else(usage)?,
            expected_verifying_key,
            skip_build,
            release_identity: release_identity.ok_or_else(usage)?,
            generation: generation.ok_or_else(usage)?,
            release_version: release_version.ok_or_else(usage)?,
            minimum_version: minimum_version.ok_or_else(usage)?,
            base_url: normalize_base_url(&base_url.ok_or_else(usage)?)?,
            published_at: published_at.ok_or_else(usage)?,
            channel,
            rollout_cohort,
            rollout_percentage,
            output_root,
            r2_bucket,
            wrangler,
            iscc,
            authenticode_tool,
            authenticode_certificate_sha1,
            authenticode_timestamp_url,
        };
        if !valid_release_identity(&config.release_identity)
            || !valid_identifier(&config.channel, 32)
            || !valid_identifier(&config.rollout_cohort, 64)
            || !valid_published_at(&config.published_at)
            || semver::Version::parse(&config.release_version).is_err()
            || semver::Version::parse(&config.minimum_version).is_err()
        {
            return Err(
                "release identity, release version, minimum version, channel, or publish time is invalid"
                    .to_string(),
            );
        }
        if cfg!(target_os = "windows") {
            let certificate = config
                .authenticode_certificate_sha1
                .as_deref()
                .ok_or_else(|| {
                    "Windows publishing requires an Authenticode certificate SHA-1 thumbprint"
                        .to_string()
                })?;
            let timestamp_url = config
                .authenticode_timestamp_url
                .as_deref()
                .ok_or_else(|| {
                    "Windows publishing requires an RFC 3161 timestamp URL".to_string()
                })?;
            if !valid_certificate_sha1(certificate) || !valid_timestamp_url(timestamp_url) {
                return Err(
                    "Windows Authenticode certificate thumbprint or timestamp URL is invalid"
                        .to_string(),
                );
            }
        }
        Ok(config)
    }
}

fn usage() -> String {
    "usage: axiusflow_release_publisher --signing-key-file <base64url-key-file> [--expected-verifying-key <base64url-public-key>] [--skip-build] --release-identity <git-head> --generation <n> --release-version <candidate-semver> --minimum-version <compatible-launcher-semver> --base-url <https-release-base> --published-at <UTC-RFC3339> [--channel stable] [--rollout-cohort all] [--rollout-percentage 100] [--output target/release-publish] [--r2-bucket <bucket>] [--wrangler <command>] [--iscc <Inno Setup compiler>] [--authenticode-tool <signtool>] --authenticode-certificate-sha1 <40-hex-thumbprint> --authenticode-timestamp-url <RFC3161-url>".to_string()
}

fn required_argument(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| format!("{flag} value is not valid UTF-8"))
}

fn run(arguments: impl Iterator<Item = OsString>) -> Result<(), String> {
    let config = PublisherConfig::parse(arguments)?;
    verify_production_publication_context(config.r2_bucket.as_deref(), |key| {
        std::env::var_os(key)
    })?;
    if config.r2_bucket.is_some() && (!config.skip_build || config.expected_verifying_key.is_none())
    {
        return Err(
            "production publication requires a separately qualified prebuilt release and an explicit expected verifying key"
                .to_string(),
        );
    }
    let repository =
        std::env::current_dir().map_err(|_| "repository directory is unavailable".to_string())?;
    if !repository.join("Cargo.toml").is_file() {
        return Err("run the release publisher from the Axiusflow repository root".to_string());
    }
    verify_repository_identity(&repository, &config.release_identity)?;
    let signing_key = read_signing_key(&config.signing_key_file)?;
    let verifying_key = signing_key.verifying_key();
    if config
        .expected_verifying_key
        .as_ref()
        .is_some_and(|expected| expected != &verifying_key)
    {
        return Err(
            "release signing key does not match the qualified public verifying key".to_string(),
        );
    }
    if !config.skip_build {
        build_release_binaries(&repository, &config, &verifying_key)?;
    }
    let binaries = release_binary_paths(&repository);
    let published = package_release(&repository, &config, &signing_key, &binaries)?;
    print_release_summary(&published);
    if let Some(bucket) = config.r2_bucket.as_deref()
        && let Err(first_error) =
            upload_release(&config, bucket, &published, &signing_key, &verifying_key)
    {
        eprintln!("Axiusflow release publisher retrying publication after: {first_error}");
        upload_release(&config, bucket, &published, &signing_key, &verifying_key).map_err(
            |second_error| {
                format!(
                    "publication failed after one bounded retry: {second_error}; first attempt: {first_error}"
                )
            },
        )?;
    }
    Ok(())
}

fn parse_verifying_key(encoded: &str) -> Result<VerifyingKey, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "release verifying key is not valid base64url".to_string())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "release verifying key must contain exactly 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| "release verifying key is invalid".to_string())
}

fn verify_production_publication_context(
    r2_bucket: Option<&str>,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Result<(), String> {
    if r2_bucket.is_none() {
        return Ok(());
    }
    for (name, expected) in [
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_WORKFLOW", "Production release"),
        ("GITHUB_EVENT_NAME", "workflow_dispatch"),
        ("GITHUB_REF", "refs/heads/main"),
        (
            "AXIUSFLOW_RELEASE_ENVIRONMENT",
            "self-hosted-release-station",
        ),
    ] {
        if environment(name).as_deref() != Some(std::ffi::OsStr::new(expected)) {
            return Err(
                "R2 publication is restricted to the self-hosted Production release GitHub Actions workflow"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn normalize_base_url(value: &str) -> Result<String, String> {
    let value = value.trim_end_matches('/');
    let Some(rest) = value.strip_prefix("https://") else {
        return Err("release base URL must use https".to_string());
    };
    if rest.is_empty() || rest.starts_with('/') || value.contains([' ', '\n', '\r', '\t', '?', '#'])
    {
        return Err("release base URL is invalid".to_string());
    }
    Ok(value.to_string())
}

fn valid_certificate_sha1(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_timestamp_url(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.is_empty() && !rest.starts_with('/') && !value.contains([' ', '\n', '\r', '\t', '#'])
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn valid_release_identity(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_published_at(value: &str) -> bool {
    valid_utc_rfc3339(value)
}

fn valid_utc_rfc3339(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.len() > 40
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes.last() != Some(&b'Z')
    {
        return false;
    }
    if bytes.len() == 21
        || (bytes.len() > 20
            && (bytes[19] != b'.'
                || bytes[20..bytes.len() - 1]
                    .iter()
                    .any(|byte| !byte.is_ascii_digit())))
    {
        return false;
    }
    let numeric = |start: usize, end: usize| {
        value
            .get(start..end)
            .and_then(|part| part.parse::<u32>().ok())
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        numeric(0, 4),
        numeric(5, 7),
        numeric(8, 10),
        numeric(11, 13),
        numeric(14, 16),
        numeric(17, 19),
    ) else {
        return false;
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day > 0 && day <= maximum_day && hour <= 23 && minute <= 59 && second <= 59
}

fn verify_repository_identity(repository: &Path, expected_identity: &str) -> Result<(), String> {
    let head = command_stdout(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository),
        "Git HEAD inspection",
    )?;
    if head.trim() != expected_identity {
        return Err("release identity must exactly match the current Git HEAD".to_string());
    }
    let status = command_stdout(
        Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=normal"])
            .current_dir(repository),
        "Git worktree inspection",
    )?;
    if !status.trim().is_empty() {
        return Err("release publisher requires a clean Git worktree".to_string());
    }
    Ok(())
}

fn command_stdout(command: &mut Command, description: &str) -> Result<String, String> {
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| format!("{description} could not be started"))?;
    if !output.status.success() {
        return Err(format!("{description} failed"));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{description} returned invalid text"))
}

fn read_signing_key(path: &Path) -> Result<SigningKey, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "release signing key file is unavailable".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 {
        return Err("release signing key file is invalid".to_string());
    }
    let mut encoded =
        fs::read(path).map_err(|_| "release signing key file could not be read".to_string())?;
    let start = encoded
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(encoded.len());
    let end = encoded
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    let decoded = URL_SAFE_NO_PAD.decode(&encoded[start..end]);
    encoded.fill(0);
    let mut decoded =
        decoded.map_err(|_| "release signing key is not valid base64url".to_string())?;
    let mut key_bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| "release signing key must contain exactly 32 bytes".to_string())?;
    decoded.fill(0);
    let key = SigningKey::from_bytes(&key_bytes);
    key_bytes.fill(0);
    Ok(key)
}

fn build_release_binaries(
    repository: &Path,
    config: &PublisherConfig,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let public_key = URL_SAFE_NO_PAD.encode(verifying_key.to_bytes());
    let generation = config.generation.to_string();
    let mut launcher = Command::new("cargo");
    launcher.args([
        "build",
        "--locked",
        "--release",
        "-p",
        "axiusflow_platform_runtime",
        "--bin",
        "axiusflow_launcher",
    ]);
    configure_release_build(&mut launcher, repository, config, &public_key, &generation);
    run_child(launcher, "release launcher build")?;

    let mut applications = Command::new("cargo");
    applications.args([
        "build",
        "--locked",
        "--release",
        "-p",
        "axiusflow_desktop",
        "--all-features",
    ]);
    configure_release_build(
        &mut applications,
        repository,
        config,
        &public_key,
        &generation,
    );
    run_child(applications, "release desktop build")
}

fn configure_release_build(
    command: &mut Command,
    repository: &Path,
    config: &PublisherConfig,
    public_key: &str,
    generation: &str,
) {
    command
        .current_dir(repository)
        .env("AXIUSFLOW_RELEASE_VERIFYING_KEY", public_key)
        .env("AXIUSFLOW_RELEASE_BASE_URL", &config.base_url)
        .env("AXIUSFLOW_BOOTSTRAP_MIN_GENERATION", generation)
        .env("AXIUSFLOW_RELEASE_IDENTITY", &config.release_identity)
        .env("AXIUSFLOW_INSTALL_GENERATION", generation)
        .stdin(Stdio::null());
    if let Some(certificate) = config.authenticode_certificate_sha1.as_deref() {
        command.env("AXIUSFLOW_AUTHENTICODE_CERT_SHA1", certificate);
    }
}

fn run_child(mut command: Command, description: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|_| format!("{description} could not be started"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{description} failed"))
    }
}

#[derive(Clone, Debug)]
struct ReleaseBinaries {
    launcher: PathBuf,
    desktop: PathBuf,
}

fn release_binary_paths(repository: &Path) -> ReleaseBinaries {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map_or_else(
            || repository.join("target"),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    repository.join(path)
                }
            },
        );
    let release = target.join("release");
    ReleaseBinaries {
        launcher: release.join(format!(
            "axiusflow_launcher{}",
            std::env::consts::EXE_SUFFIX
        )),
        desktop: release.join(format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX)),
    }
}

#[derive(Debug)]
struct UploadObject {
    local_path: PathBuf,
    object_key: String,
    content_type: &'static str,
    requires_authenticode: bool,
    requires_provenance_signature: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseProvenanceArtifact {
    name: String,
    size: u64,
    sha256_b64url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseProvenance {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
    channel: String,
    platform: String,
    architecture: String,
    rollout: RolloutMetadata,
    artifacts: Vec<ReleaseProvenanceArtifact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReleaseProvenance {
    provenance: ReleaseProvenance,
    signature_b64url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRetirement {
    schema_version: u32,
    target_release_identity: String,
    target_install_generation: u64,
    predecessor: Option<SignedReleaseManifest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReleaseRetirement {
    retirement: ReleaseRetirement,
    signature_b64url: String,
}

#[derive(Debug)]
struct PublishedRelease {
    release_directory: PathBuf,
    manifest_path: PathBuf,
    channel_path: PathBuf,
    immutable_objects: Vec<UploadObject>,
    channel_object: UploadObject,
}

#[derive(Debug)]
enum RemoteChannelProgression {
    Empty,
    Predecessor(ReleaseChannelPointer),
    Current(ReleaseChannelPointer),
    CommittedGenerationMismatch(ReleaseChannelPointer),
}

#[allow(clippy::too_many_lines)]
fn package_release(
    repository: &Path,
    config: &PublisherConfig,
    signing_key: &SigningKey,
    binaries: &ReleaseBinaries,
) -> Result<PublishedRelease, String> {
    let platform = std::env::consts::OS;
    let architecture = std::env::consts::ARCH;
    let release_key = format!("{}-{}", config.generation, config.release_identity);
    let release_public_root = format!("{platform}/{architecture}/{release_key}");
    let release_object_root = format!("releases/{platform}/{architecture}/{release_key}");
    let release_directory = config.output_root.join(&release_object_root);
    if release_directory.exists() {
        return Err("immutable local release directory already exists".to_string());
    }
    fs::create_dir_all(&release_directory)
        .map_err(|_| "release output directory could not be created".to_string())?;

    let setup_name = format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX);
    let launcher_name = format!("axiusflow_launcher{}", std::env::consts::EXE_SUFFIX);
    let desktop_name = format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX);
    let rollback_compatibility_name = ROLLBACK_COMPATIBILITY_FILENAME;
    let setup_path = release_directory.join(&setup_name);
    let launcher_path = release_directory.join(&launcher_name);
    let desktop_path = release_directory.join(&desktop_name);
    let rollback_compatibility_path = release_directory.join(rollback_compatibility_name);
    copy_release_binary(&binaries.launcher, &launcher_path)?;
    copy_release_binary(&binaries.desktop, &desktop_path)?;
    if cfg!(target_os = "windows") && !cfg!(test) {
        sign_authenticode_file(config, &launcher_path)?;
        sign_authenticode_file(config, &desktop_path)?;
    }
    write_json_new(
        &rollback_compatibility_path,
        &RollbackCompatibilityMetadata::current(),
    )?;

    let mut files = vec![
        release_file(
            ReleaseFileRole::Desktop,
            &desktop_path,
            &desktop_name,
            &format!("{release_public_root}/{desktop_name}"),
            &config.base_url,
            true,
        )?,
        release_file(
            ReleaseFileRole::RuntimeAsset,
            &launcher_path,
            &launcher_name,
            &format!("{release_public_root}/{launcher_name}"),
            &config.base_url,
            true,
        )?,
        release_file(
            ReleaseFileRole::RuntimeAsset,
            &rollback_compatibility_path,
            rollback_compatibility_name,
            &format!("{release_public_root}/{rollback_compatibility_name}"),
            &config.base_url,
            false,
        )?,
    ];
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = ReleaseManifest {
        schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
        release_identity: config.release_identity.clone(),
        install_generation: config.generation,
        channel: config.channel.clone(),
        minimum_version: config.minimum_version.clone(),
        platform: platform.to_string(),
        architecture: architecture.to_string(),
        files,
        rollout: RolloutMetadata {
            cohort: config.rollout_cohort.clone(),
            percentage: config.rollout_percentage,
        },
    };
    let signed = sign_release_manifest(manifest, signing_key)
        .map_err(|error| format!("release manifest signing failed: {error}"))?;
    let manifest_path = release_directory.join("manifest.json");
    write_json_new(&manifest_path, &signed)?;

    if cfg!(target_os = "windows") {
        if cfg!(test) {
            // Unit packaging tests do not invoke external installer tooling,
            // but production uses the same path and contract below.
            let _ = &config.iscc;
            copy_release_binary(&binaries.launcher, &setup_path)?;
        } else {
            compile_windows_installer(
                repository,
                config,
                &launcher_path,
                &manifest_path,
                &desktop_path,
                &rollback_compatibility_path,
                &setup_path,
            )?;
            sign_authenticode_file(config, &setup_path)?;
        }
    } else {
        // The public native installer is currently a Windows product. Keep
        // non-Windows publisher tests/builds viable without inventing another
        // packaging format here.
        copy_release_binary(&launcher_path, &setup_path)?;
    }

    let manifest_url = format!("{}/{release_public_root}/manifest.json", config.base_url);
    let setup_metadata = fs::metadata(&setup_path)
        .map_err(|_| "packaged setup metadata is unavailable".to_string())?;
    let installer = ReleaseInstallerMetadata {
        filename: setup_name.clone(),
        url: format!("{}/{release_public_root}/{setup_name}", config.base_url),
        size: setup_metadata.len(),
        sha256_b64url: file_sha256_base64url(&setup_path)?,
    };
    let provenance_path = release_directory.join("provenance.json");
    let provenance = sign_release_provenance(
        ReleaseProvenance {
            schema_version: RELEASE_PROVENANCE_SCHEMA_VERSION,
            release_identity: config.release_identity.clone(),
            install_generation: config.generation,
            channel: config.channel.clone(),
            platform: platform.to_string(),
            architecture: architecture.to_string(),
            rollout: RolloutMetadata {
                cohort: config.rollout_cohort.clone(),
                percentage: config.rollout_percentage,
            },
            artifacts: vec![
                provenance_artifact(&setup_name, &setup_path)?,
                provenance_artifact("manifest.json", &manifest_path)?,
                provenance_artifact(&launcher_name, &launcher_path)?,
                provenance_artifact(&desktop_name, &desktop_path)?,
                provenance_artifact(rollback_compatibility_name, &rollback_compatibility_path)?,
            ],
        },
        signing_key,
    )?;
    write_json_new(&provenance_path, &provenance)?;
    verify_release_provenance_file(&provenance_path, &signing_key.verifying_key())?;
    let channel = ReleaseChannelPointer {
        schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
        channel: config.channel.clone(),
        platform: platform.to_string(),
        architecture: architecture.to_string(),
        release_identity: config.release_identity.clone(),
        install_generation: config.generation,
        version: config.release_version.clone(),
        published_at: config.published_at.clone(),
        manifest_url,
        signed_release: signed,
        installer,
    };
    let channel_object_key = format!("channels/{}/{platform}/{architecture}.json", config.channel);
    let channel_path = config.output_root.join(&channel_object_key);
    write_json_atomic(&channel_path, &channel)?;

    let executable_content_type = if cfg!(target_os = "windows") {
        "application/vnd.microsoft.portable-executable"
    } else {
        "application/octet-stream"
    };
    let immutable_objects = vec![
        UploadObject {
            local_path: setup_path,
            object_key: format!("{release_object_root}/{setup_name}"),
            content_type: executable_content_type,
            requires_authenticode: cfg!(target_os = "windows"),
            requires_provenance_signature: false,
        },
        UploadObject {
            local_path: launcher_path,
            object_key: format!("{release_object_root}/{launcher_name}"),
            content_type: executable_content_type,
            requires_authenticode: cfg!(target_os = "windows"),
            requires_provenance_signature: false,
        },
        UploadObject {
            local_path: desktop_path,
            object_key: format!("{release_object_root}/{desktop_name}"),
            content_type: executable_content_type,
            requires_authenticode: cfg!(target_os = "windows"),
            requires_provenance_signature: false,
        },
        UploadObject {
            local_path: manifest_path.clone(),
            object_key: format!("{release_object_root}/manifest.json"),
            content_type: "application/json",
            requires_authenticode: false,
            requires_provenance_signature: false,
        },
        UploadObject {
            local_path: rollback_compatibility_path,
            object_key: format!("{release_object_root}/{rollback_compatibility_name}"),
            content_type: "application/json",
            requires_authenticode: false,
            requires_provenance_signature: false,
        },
        UploadObject {
            local_path: provenance_path,
            object_key: format!("{release_object_root}/provenance.json"),
            content_type: "application/json",
            requires_authenticode: false,
            requires_provenance_signature: true,
        },
    ];
    Ok(PublishedRelease {
        release_directory,
        manifest_path,
        channel_path: channel_path.clone(),
        immutable_objects,
        channel_object: UploadObject {
            local_path: channel_path,
            object_key: channel_object_key,
            content_type: "application/json",
            requires_authenticode: false,
            requires_provenance_signature: false,
        },
    })
}

fn copy_release_binary(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(source).map_err(|_| "built release binary is missing".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
        return Err("built release binary is invalid".to_string());
    }
    fs::copy(source, destination)
        .map_err(|_| "release binary could not be packaged".to_string())?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|_| "packaged release binary could not be committed".to_string())?;
    Ok(())
}

fn provenance_artifact(name: &str, path: &Path) -> Result<ReleaseProvenanceArtifact, String> {
    let size = fs::metadata(path)
        .map_err(|_| "release provenance artifact metadata is unavailable".to_string())?
        .len();
    if size == 0 {
        return Err("release provenance artifact is empty".to_string());
    }
    Ok(ReleaseProvenanceArtifact {
        name: name.to_string(),
        size,
        sha256_b64url: file_sha256_base64url(path)?,
    })
}

fn sign_release_provenance(
    provenance: ReleaseProvenance,
    key: &SigningKey,
) -> Result<SignedReleaseProvenance, String> {
    validate_release_provenance(&provenance)?;
    let canonical = serde_json::to_vec(&provenance)
        .map_err(|_| "release provenance could not be serialized".to_string())?;
    let mut message =
        Vec::with_capacity(RELEASE_PROVENANCE_SIGNATURE_DOMAIN.len() + canonical.len());
    message.extend_from_slice(RELEASE_PROVENANCE_SIGNATURE_DOMAIN);
    message.extend_from_slice(&canonical);
    Ok(SignedReleaseProvenance {
        provenance,
        signature_b64url: URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes()),
    })
}

fn verify_release_provenance_file(path: &Path, key: &VerifyingKey) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|_| "signed release provenance metadata is unavailable".to_string())?;
    if metadata.len() == 0 || metadata.len() > MAXIMUM_CHANNEL_BYTES {
        return Err("signed release provenance size is invalid".to_string());
    }
    let signed: SignedReleaseProvenance = serde_json::from_slice(
        &fs::read(path).map_err(|_| "signed release provenance could not be read".to_string())?,
    )
    .map_err(|_| "signed release provenance is malformed".to_string())?;
    validate_release_provenance(&signed.provenance)?;
    let canonical = serde_json::to_vec(&signed.provenance)
        .map_err(|_| "release provenance could not be serialized".to_string())?;
    let mut message =
        Vec::with_capacity(RELEASE_PROVENANCE_SIGNATURE_DOMAIN.len() + canonical.len());
    message.extend_from_slice(RELEASE_PROVENANCE_SIGNATURE_DOMAIN);
    message.extend_from_slice(&canonical);
    let signature = URL_SAFE_NO_PAD
        .decode(&signed.signature_b64url)
        .map_err(|_| "release provenance signature is invalid".to_string())?;
    let signature = Signature::from_slice(&signature)
        .map_err(|_| "release provenance signature is invalid".to_string())?;
    key.verify(&message, &signature)
        .map_err(|_| "release provenance signature verification failed".to_string())
}

fn sign_release_retirement(
    retirement: ReleaseRetirement,
    key: &SigningKey,
) -> Result<SignedReleaseRetirement, String> {
    let canonical = canonical_release_retirement(&retirement)?;
    let mut message =
        Vec::with_capacity(RELEASE_RETIREMENT_SIGNATURE_DOMAIN.len() + canonical.len());
    message.extend_from_slice(RELEASE_RETIREMENT_SIGNATURE_DOMAIN);
    message.extend_from_slice(&canonical);
    Ok(SignedReleaseRetirement {
        retirement,
        signature_b64url: URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes()),
    })
}

fn verify_release_retirement(
    signed: &SignedReleaseRetirement,
    target: &ReleaseManifest,
    key: &VerifyingKey,
) -> Result<(), String> {
    let canonical = canonical_release_retirement(&signed.retirement)?;
    let mut message =
        Vec::with_capacity(RELEASE_RETIREMENT_SIGNATURE_DOMAIN.len() + canonical.len());
    message.extend_from_slice(RELEASE_RETIREMENT_SIGNATURE_DOMAIN);
    message.extend_from_slice(&canonical);
    let signature = URL_SAFE_NO_PAD
        .decode(&signed.signature_b64url)
        .map_err(|_| "release retirement signature is invalid".to_string())?;
    let signature = Signature::from_slice(&signature)
        .map_err(|_| "release retirement signature is invalid".to_string())?;
    key.verify(&message, &signature)
        .map_err(|_| "release retirement signature verification failed".to_string())?;
    if signed.retirement.target_release_identity != target.release_identity
        || signed.retirement.target_install_generation != target.install_generation
    {
        return Err("release retirement target does not match the published release".to_string());
    }
    if let Some(predecessor) = &signed.retirement.predecessor {
        verify_release_manifest_signature(predecessor, key)
            .map_err(|_| "release retirement predecessor signature is invalid".to_string())?;
        validate_predecessor_manifest_shape(&predecessor.manifest)?;
        if predecessor.manifest.install_generation >= target.install_generation
            || predecessor.manifest.release_identity == target.release_identity
            || predecessor.manifest.channel != target.channel
            || predecessor.manifest.platform != target.platform
            || predecessor.manifest.architecture != target.architecture
        {
            return Err("release retirement predecessor is inconsistent".to_string());
        }
    }
    Ok(())
}

fn canonical_release_retirement(retirement: &ReleaseRetirement) -> Result<Vec<u8>, String> {
    if retirement.schema_version != RELEASE_RETIREMENT_SCHEMA_VERSION
        || !valid_release_identity(&retirement.target_release_identity)
        || retirement.target_install_generation == 0
    {
        return Err("release retirement shape is invalid".to_string());
    }
    let canonical = serde_json::to_vec(retirement)
        .map_err(|_| "release retirement could not be serialized".to_string())?;
    if canonical.len() as u64 > MAXIMUM_RETIREMENT_BYTES {
        return Err("release retirement exceeds the size bound".to_string());
    }
    Ok(canonical)
}

fn validate_release_provenance(provenance: &ReleaseProvenance) -> Result<(), String> {
    if provenance.schema_version != RELEASE_PROVENANCE_SCHEMA_VERSION
        || !valid_release_identity(&provenance.release_identity)
        || provenance.install_generation == 0
        || !valid_identifier(&provenance.channel, 32)
        || !valid_identifier(&provenance.platform, 32)
        || !valid_identifier(&provenance.architecture, 32)
        || !valid_identifier(&provenance.rollout.cohort, 64)
        || provenance.rollout.percentage > 100
        || provenance.artifacts.len() != 5
    {
        return Err("release provenance shape is invalid".to_string());
    }
    let expected_names = BTreeSet::from([
        format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX),
        "manifest.json".to_string(),
        format!("axiusflow_launcher{}", std::env::consts::EXE_SUFFIX),
        format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX),
        ROLLBACK_COMPATIBILITY_FILENAME.to_string(),
    ]);
    let mut actual_names = BTreeSet::new();
    for artifact in &provenance.artifacts {
        let digest = URL_SAFE_NO_PAD
            .decode(&artifact.sha256_b64url)
            .map_err(|_| "release provenance artifact hash is invalid".to_string())?;
        if !valid_identifier(&artifact.name, 128)
            || artifact.size == 0
            || digest.len() != 32
            || !actual_names.insert(artifact.name.clone())
        {
            return Err("release provenance artifact is invalid".to_string());
        }
    }
    if actual_names != expected_names {
        return Err("release provenance artifact inventory is incomplete".to_string());
    }
    Ok(())
}

fn authenticode_settings(config: &PublisherConfig) -> Result<(&str, &str), String> {
    let certificate = config
        .authenticode_certificate_sha1
        .as_deref()
        .ok_or_else(|| "Authenticode certificate thumbprint is unavailable".to_string())?;
    let timestamp_url = config
        .authenticode_timestamp_url
        .as_deref()
        .ok_or_else(|| "Authenticode timestamp URL is unavailable".to_string())?;
    if !valid_certificate_sha1(certificate) || !valid_timestamp_url(timestamp_url) {
        return Err("Authenticode signing configuration is invalid".to_string());
    }
    Ok((certificate, timestamp_url))
}

fn sign_authenticode_file(config: &PublisherConfig, path: &Path) -> Result<(), String> {
    let (certificate, timestamp_url) = authenticode_settings(config)?;
    let mut command = Command::new(&config.authenticode_tool);
    command
        .args([
            "sign",
            "/sha1",
            certificate,
            "/fd",
            "SHA256",
            "/tr",
            timestamp_url,
            "/td",
            "SHA256",
            "/v",
        ])
        .arg(path)
        .stdin(Stdio::null());
    run_child(command, "Authenticode signing")?;
    verify_authenticode_file(config, path)
}

fn verify_authenticode_file(config: &PublisherConfig, path: &Path) -> Result<(), String> {
    let (certificate, _) = authenticode_settings(config)?;
    let mut signtool = Command::new(&config.authenticode_tool);
    signtool
        .args(["verify", "/pa", "/all", "/v"])
        .arg(path)
        .stdin(Stdio::null());
    run_child(signtool, "Authenticode verification")?;

    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            VERIFY_AUTHENTICODE_METADATA,
        ])
        .env("AXIUSFLOW_AUTHENTICODE_PATH", path)
        .env("AXIUSFLOW_AUTHENTICODE_CERT_SHA1", certificate)
        .stdin(Stdio::null())
        .status()
        .map_err(|_| "Authenticode metadata verification could not be started".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(
            "Authenticode signature must be valid, use the intended publisher certificate, and carry an RFC 3161 timestamp"
                .to_string(),
        )
    }
}

fn compile_windows_installer(
    repository: &Path,
    config: &PublisherConfig,
    launcher_path: &Path,
    manifest_path: &Path,
    desktop_path: &Path,
    rollback_compatibility_path: &Path,
    setup_path: &Path,
) -> Result<(), String> {
    let script = repository.join("tools/windows/axiusflow_setup.iss");
    let icon = repository.join("apps/desktop/assets/brand_assets/axiusflow.ico");
    for input in [
        script.as_path(),
        icon.as_path(),
        launcher_path,
        manifest_path,
        desktop_path,
        rollback_compatibility_path,
    ] {
        let metadata = fs::symlink_metadata(input)
            .map_err(|_| "Windows installer input is unavailable".to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
            return Err("Windows installer input is invalid".to_string());
        }
    }
    if setup_path.exists() {
        return Err("immutable Windows installer already exists".to_string());
    }
    let output_dir = setup_path
        .parent()
        .ok_or_else(|| "Windows installer output path is invalid".to_string())?;
    let status = Command::new(&config.iscc)
        .arg("/Qp")
        .arg(format!("/DAppVersion={}", config.release_version))
        .arg(format!("/DLauncherPath={}", launcher_path.display()))
        .arg(format!("/DManifestPath={}", manifest_path.display()))
        .arg(format!("/DDesktopPath={}", desktop_path.display()))
        .arg(format!(
            "/DRollbackCompatibilityPath={}",
            rollback_compatibility_path.display()
        ))
        .arg(format!("/DIconPath={}", icon.display()))
        .arg(format!("/DOutputDir={}", output_dir.display()))
        .arg(&script)
        .current_dir(repository)
        .stdin(Stdio::null())
        .status()
        .map_err(|_| "Inno Setup compiler could not be started".to_string())?;
    if !status.success() {
        return Err("Inno Setup compiler failed".to_string());
    }
    let metadata = fs::symlink_metadata(setup_path)
        .map_err(|_| "Inno Setup did not emit Axiusflow-Setup.exe".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
        return Err("Inno Setup emitted an invalid installer".to_string());
    }
    Ok(())
}

fn release_file(
    role: ReleaseFileRole,
    path: &Path,
    name: &str,
    object_key: &str,
    base_url: &str,
    executable: bool,
) -> Result<ReleaseFile, String> {
    let metadata = fs::metadata(path)
        .map_err(|_| "packaged release binary metadata is unavailable".to_string())?;
    Ok(ReleaseFile {
        role,
        path: name.to_string(),
        url: format!("{base_url}/{object_key}"),
        size: metadata.len(),
        sha256: file_sha256_base64url(path)?,
        executable,
    })
}

fn file_sha256_base64url(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|_| "release artifact could not be opened for hashing".to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "release artifact could not be hashed".to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(URL_SAFE_NO_PAD.encode(digest.finalize()))
}

fn write_json_new(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| "release metadata could not be serialized".to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "immutable release metadata already exists".to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "release metadata could not be committed".to_string())
}

fn write_json_new_or_exact(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| "release metadata could not be serialized".to_string())?;
    write_bytes_new_or_exact(path, &bytes)
}

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "channel output path is invalid".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|_| "channel output directory could not be created".to_string())?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| "channel metadata could not be serialized".to_string())?;
    let staging = path.with_extension("json.next");
    let _ = fs::remove_file(&staging);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging)
        .map_err(|_| "channel staging file could not be created".to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "channel staging file could not be committed".to_string())?;
    replace_file_atomically(&staging, path)
        .map_err(|_| "channel metadata could not be committed".to_string())
}

fn print_release_summary(release: &PublishedRelease) {
    println!("release_directory={}", release.release_directory.display());
    println!("signed_manifest={}", release.manifest_path.display());
    println!("channel_pointer={}", release.channel_path.display());
    for object in &release.immutable_objects {
        println!("immutable_object={}", object.object_key);
    }
    println!("channel_object={}", release.channel_object.object_key);
}

#[allow(clippy::too_many_lines)]
fn upload_release(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    signing_key: &SigningKey,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    if !valid_identifier(bucket, 128) {
        return Err("R2 bucket name is invalid".to_string());
    }
    let verify_root = config.output_root.join(".r2-verify");
    fs::create_dir_all(&verify_root)
        .map_err(|_| "R2 verification directory could not be created".to_string())?;
    let progression =
        verify_remote_channel_progression(config, bucket, release, verifying_key, &verify_root)?;
    if matches!(
        progression,
        RemoteChannelProgression::Current(_)
            | RemoteChannelProgression::CommittedGenerationMismatch(_)
    ) {
        let (current, exact_candidate) = match progression {
            RemoteChannelProgression::Current(current) => (current, true),
            RemoteChannelProgression::CommittedGenerationMismatch(current) => (current, false),
            _ => unreachable!(),
        };
        verify_public_channel_pointer(config, &current)?;
        verify_current_installer_alias(config, &current)?;
        let retirement = fetch_remote_release_retirement(
            config,
            bucket,
            &current.signed_release.manifest,
            verifying_key,
            &verify_root,
        )?;
        if let Some(predecessor) = retirement.retirement.predecessor.as_ref() {
            retire_predecessor_release(config, bucket, predecessor, &verify_root)?;
        }
        let _ = fs::remove_dir(&verify_root);
        if exact_candidate {
            return Ok(());
        }
        return Err(
            "stable channel already committed this generation with a different signed candidate"
                .to_string(),
        );
    }
    let predecessor = match progression {
        RemoteChannelProgression::Empty => None,
        RemoteChannelProgression::Predecessor(predecessor) => Some(predecessor),
        RemoteChannelProgression::Current(_)
        | RemoteChannelProgression::CommittedGenerationMismatch(_) => unreachable!(),
    };
    let retirement_object =
        build_release_retirement_object(release, predecessor.as_ref(), signing_key, verifying_key)?;
    let block_plan_object = if let Some(predecessor) = predecessor.as_ref() {
        build_optional_block_plan_object(
            config,
            bucket,
            release,
            predecessor,
            signing_key,
            &verify_root,
        )?
    } else {
        None
    };
    if block_plan_object.is_none() {
        let target: SignedReleaseManifest = serde_json::from_slice(
            &fs::read(&release.manifest_path)
                .map_err(|_| "candidate signed manifest could not be read".to_string())?,
        )
        .map_err(|_| "candidate signed manifest is malformed".to_string())?;
        let stale_block_plan = format!(
            "{}/{}",
            release_object_root(&target.manifest),
            BLOCK_PLAN_FILENAME
        );
        remove_remote_object_and_verify_absent(config, bucket, &stale_block_plan, &verify_root)?;
        verify_public_object_absent(config, &stale_block_plan)?;
    }
    for object in release
        .immutable_objects
        .iter()
        .chain(block_plan_object.iter())
        .chain(std::iter::once(&retirement_object))
    {
        ensure_wrangler_size(object)?;
        if let RemoteObject::Downloaded(downloaded) =
            wrangler_get(config, bucket, &object.object_key, &verify_root)?
        {
            let exact = verify_uploaded_object(config, object, &downloaded, verifying_key).is_ok();
            let _ = fs::remove_file(downloaded);
            if exact {
                verify_public_object(config, object)?;
                continue;
            }
            return Err(format!(
                "immutable R2 object already exists with different content: {}",
                object.object_key
            ));
        }
        wrangler_put(
            config,
            bucket,
            object,
            "public, max-age=31536000, immutable",
        )?;
        let RemoteObject::Downloaded(downloaded) =
            wrangler_get(config, bucket, &object.object_key, &verify_root)?
        else {
            return Err(format!(
                "uploaded R2 object could not be read back: {}",
                object.object_key
            ));
        };
        verify_uploaded_object(config, object, &downloaded, verifying_key)?;
        if object
            .object_key
            .ends_with(&format!("/{RELEASE_RETIREMENT_FILENAME}"))
        {
            verify_release_retirement_file(&downloaded, &release.manifest_path, verifying_key)?;
        }
        if object
            .object_key
            .ends_with(&format!("/{BLOCK_PLAN_FILENAME}"))
        {
            let predecessor = predecessor
                .as_ref()
                .ok_or_else(|| "release block plan has no authenticated predecessor".to_string())?;
            verify_block_plan_readback(
                &downloaded,
                predecessor,
                &release.manifest_path,
                verifying_key,
            )?;
        }
        fs::remove_file(downloaded)
            .map_err(|_| "R2 verification artifact could not be removed".to_string())?;
        verify_public_object(config, object)?;
    }
    verify_candidate_release_for_publication(release, verifying_key)?;
    ensure_wrangler_size(&release.channel_object)?;
    // Publish the mutable pointer last so clients can never discover a
    // release before every referenced immutable object is present.
    wrangler_put(
        config,
        bucket,
        &release.channel_object,
        "no-cache, max-age=0, must-revalidate",
    )?;
    let RemoteObject::Downloaded(downloaded_channel) = wrangler_get(
        config,
        bucket,
        &release.channel_object.object_key,
        &verify_root,
    )?
    else {
        return Err("published stable channel could not be read back from R2".to_string());
    };
    verify_uploaded_object(
        config,
        &release.channel_object,
        &downloaded_channel,
        verifying_key,
    )?;
    fs::remove_file(downloaded_channel)
        .map_err(|_| "stable channel verification artifact could not be removed".to_string())?;
    verify_public_object(config, &release.channel_object)?;
    let published_channel: ReleaseChannelPointer = serde_json::from_slice(
        &fs::read(&release.channel_path)
            .map_err(|_| "candidate channel metadata could not be read".to_string())?,
    )
    .map_err(|_| "candidate channel metadata is malformed".to_string())?;
    verify_current_installer_alias(config, &published_channel)?;
    if let Some(predecessor) = predecessor.as_ref() {
        retire_predecessor_release(config, bucket, &predecessor.signed_release, &verify_root)?;
    }
    let _ = fs::remove_dir(verify_root);
    Ok(())
}

fn verify_remote_channel_progression(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    verifying_key: &VerifyingKey,
    verify_root: &Path,
) -> Result<RemoteChannelProgression, String> {
    let candidate: ReleaseChannelPointer = serde_json::from_slice(
        &fs::read(&release.channel_path)
            .map_err(|_| "candidate channel metadata could not be read".to_string())?,
    )
    .map_err(|_| "candidate channel metadata is malformed".to_string())?;
    let remote = wrangler_get(
        config,
        bucket,
        &release.channel_object.object_key,
        verify_root,
    )?;
    let RemoteObject::Downloaded(path) = remote else {
        return Ok(RemoteChannelProgression::Empty);
    };
    let result = (|| {
        let metadata = fs::metadata(&path)
            .map_err(|_| "existing stable channel metadata is unavailable".to_string())?;
        if metadata.len() > MAXIMUM_CHANNEL_BYTES {
            return Err("existing stable channel exceeds the metadata size bound".to_string());
        }
        let channel: ReleaseChannelPointer = serde_json::from_slice(
            &fs::read(&path)
                .map_err(|_| "existing stable channel could not be read".to_string())?,
        )
        .map_err(|_| "existing stable channel is malformed".to_string())?;
        validate_predecessor_channel(config, &channel, verifying_key)?;
        classify_remote_channel_progression(config, channel, &candidate)
    })();
    let _ = fs::remove_file(path);
    result
}

fn validate_predecessor_channel(
    config: &PublisherConfig,
    channel: &ReleaseChannelPointer,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    // A predecessor is an authenticated block source and monotonic channel
    // witness, not a candidate for current installation. Older signed release
    // shapes can therefore remain valid predecessors even after current local
    // policy has retired those shapes.
    verify_release_manifest_signature(&channel.signed_release, verifying_key)
        .map_err(|_| "existing stable channel signed manifest is invalid".to_string())?;
    let manifest = &channel.signed_release.manifest;
    validate_predecessor_manifest_shape(manifest)?;
    if channel.schema_version != RELEASE_CHANNEL_SCHEMA_VERSION
        || channel.channel != config.channel
        || channel.channel != manifest.channel
        || channel.platform != std::env::consts::OS
        || channel.platform != manifest.platform
        || channel.architecture != std::env::consts::ARCH
        || channel.architecture != manifest.architecture
        || channel.install_generation != manifest.install_generation
        || channel.release_identity != manifest.release_identity
        || !valid_published_at(&channel.published_at)
        || channel.version.is_empty()
        || channel.version.len() > 64
    {
        return Err("existing stable channel identity is inconsistent".to_string());
    }
    let expected_manifest_url = format!(
        "{}/{}/{}/{}-{}/manifest.json",
        config.base_url,
        manifest.platform,
        manifest.architecture,
        manifest.install_generation,
        manifest.release_identity
    );
    if channel.manifest_url != expected_manifest_url {
        return Err("existing stable channel manifest URL is inconsistent".to_string());
    }
    let expected_installer_name = format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX);
    let expected_installer_url = format!(
        "{}/{}/{}/{}-{}/{}",
        config.base_url,
        manifest.platform,
        manifest.architecture,
        manifest.install_generation,
        manifest.release_identity,
        expected_installer_name
    );
    let installer_digest = URL_SAFE_NO_PAD
        .decode(&channel.installer.sha256_b64url)
        .map_err(|_| "existing stable channel installer hash is invalid".to_string())?;
    if channel.installer.filename != expected_installer_name
        || channel.installer.url != expected_installer_url
        || channel.installer.size == 0
        || installer_digest.len() != 32
    {
        return Err("existing stable channel installer metadata is inconsistent".to_string());
    }
    Ok(())
}

fn validate_predecessor_manifest_shape(manifest: &ReleaseManifest) -> Result<(), String> {
    if manifest.schema_version == 0
        || manifest.install_generation == 0
        || !valid_release_identity(&manifest.release_identity)
        || !valid_identifier(&manifest.channel, 32)
        || !valid_identifier(&manifest.platform, 32)
        || !valid_identifier(&manifest.architecture, 32)
        || !valid_identifier(&manifest.rollout.cohort, 64)
        || manifest.rollout.percentage > 100
        || manifest.files.is_empty()
        || manifest.files.len() > 256
    {
        return Err("existing stable channel signed manifest shape is invalid".to_string());
    }
    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        let path = Path::new(&file.path);
        let digest = URL_SAFE_NO_PAD
            .decode(&file.sha256)
            .map_err(|_| "existing stable channel file hash is invalid".to_string())?;
        if file.size == 0
            || digest.len() != 32
            || !file.url.starts_with("https://")
            || !path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
            || !paths.insert(file.path.as_str())
        {
            return Err("existing stable channel signed manifest shape is invalid".to_string());
        }
    }
    Ok(())
}

fn release_object_root(manifest: &ReleaseManifest) -> String {
    format!(
        "releases/{}/{}/{}-{}",
        manifest.platform,
        manifest.architecture,
        manifest.install_generation,
        manifest.release_identity
    )
}

fn build_release_retirement_object(
    release: &PublishedRelease,
    predecessor: Option<&ReleaseChannelPointer>,
    signing_key: &SigningKey,
    verifying_key: &VerifyingKey,
) -> Result<UploadObject, String> {
    let target: SignedReleaseManifest = serde_json::from_slice(
        &fs::read(&release.manifest_path)
            .map_err(|_| "candidate signed manifest could not be read".to_string())?,
    )
    .map_err(|_| "candidate signed manifest is malformed".to_string())?;
    let signed = sign_release_retirement(
        ReleaseRetirement {
            schema_version: RELEASE_RETIREMENT_SCHEMA_VERSION,
            target_release_identity: target.manifest.release_identity.clone(),
            target_install_generation: target.manifest.install_generation,
            predecessor: predecessor.map(|channel| channel.signed_release.clone()),
        },
        signing_key,
    )?;
    verify_release_retirement(&signed, &target.manifest, verifying_key)?;
    let path = release.release_directory.join(RELEASE_RETIREMENT_FILENAME);
    write_json_new_or_exact(&path, &signed)?;
    Ok(UploadObject {
        local_path: path,
        object_key: format!(
            "{}/{}",
            release_object_root(&target.manifest),
            RELEASE_RETIREMENT_FILENAME
        ),
        content_type: "application/json",
        requires_authenticode: false,
        requires_provenance_signature: false,
    })
}

fn verify_release_retirement_file(
    path: &Path,
    target_manifest_path: &Path,
    verifying_key: &VerifyingKey,
) -> Result<SignedReleaseRetirement, String> {
    let metadata = fs::metadata(path)
        .map_err(|_| "signed release retirement metadata is unavailable".to_string())?;
    if metadata.len() == 0 || metadata.len() > MAXIMUM_RETIREMENT_BYTES {
        return Err("signed release retirement size is invalid".to_string());
    }
    let target: SignedReleaseManifest = serde_json::from_slice(
        &fs::read(target_manifest_path)
            .map_err(|_| "candidate signed manifest could not be read".to_string())?,
    )
    .map_err(|_| "candidate signed manifest is malformed".to_string())?;
    let signed: SignedReleaseRetirement = serde_json::from_slice(
        &fs::read(path).map_err(|_| "signed release retirement could not be read".to_string())?,
    )
    .map_err(|_| "signed release retirement is malformed".to_string())?;
    verify_release_retirement(&signed, &target.manifest, verifying_key)?;
    Ok(signed)
}

fn fetch_remote_release_retirement(
    config: &PublisherConfig,
    bucket: &str,
    target: &ReleaseManifest,
    verifying_key: &VerifyingKey,
    verify_root: &Path,
) -> Result<SignedReleaseRetirement, String> {
    let object_key = format!(
        "{}/{}",
        release_object_root(target),
        RELEASE_RETIREMENT_FILENAME
    );
    let RemoteObject::Downloaded(path) = wrangler_get(config, bucket, &object_key, verify_root)?
    else {
        return Err("published release retirement record is missing".to_string());
    };
    let result = (|| {
        let metadata = fs::metadata(&path)
            .map_err(|_| "signed release retirement metadata is unavailable".to_string())?;
        if metadata.len() == 0 || metadata.len() > MAXIMUM_RETIREMENT_BYTES {
            return Err("signed release retirement size is invalid".to_string());
        }
        let signed: SignedReleaseRetirement = serde_json::from_slice(
            &fs::read(&path)
                .map_err(|_| "signed release retirement could not be read".to_string())?,
        )
        .map_err(|_| "signed release retirement is malformed".to_string())?;
        verify_release_retirement(&signed, target, verifying_key)?;
        Ok(signed)
    })();
    let _ = fs::remove_file(path);
    result
}

fn classify_remote_channel_progression(
    config: &PublisherConfig,
    channel: ReleaseChannelPointer,
    candidate: &ReleaseChannelPointer,
) -> Result<RemoteChannelProgression, String> {
    if channel.install_generation > config.generation {
        return Err(format!(
            "stable channel generation {} is newer than candidate generation {}; publication must use a newer generation",
            channel.install_generation, config.generation
        ));
    }
    if channel.install_generation == config.generation {
        let manifest = &channel.signed_release.manifest;
        if channel.release_identity != config.release_identity
            || channel.version != config.release_version
            || manifest.minimum_version != config.minimum_version
            || manifest.rollout.cohort != config.rollout_cohort
            || manifest.rollout.percentage != config.rollout_percentage
        {
            return Err(
                "stable channel already uses the candidate generation for different release metadata"
                    .to_string(),
            );
        }
        let exact_candidate = channel.schema_version == candidate.schema_version
            && channel.channel == candidate.channel
            && channel.platform == candidate.platform
            && channel.architecture == candidate.architecture
            && channel.release_identity == candidate.release_identity
            && channel.install_generation == candidate.install_generation
            && channel.version == candidate.version
            && channel.manifest_url == candidate.manifest_url
            && channel.signed_release == candidate.signed_release
            && channel.installer == candidate.installer;
        if !exact_candidate {
            return Ok(RemoteChannelProgression::CommittedGenerationMismatch(
                channel,
            ));
        }
        return Ok(RemoteChannelProgression::Current(channel));
    }
    Ok(RemoteChannelProgression::Predecessor(channel))
}

fn predecessor_release_object_keys(predecessor: &SignedReleaseManifest) -> Vec<String> {
    let root = release_object_root(&predecessor.manifest);
    let mut keys = BTreeSet::new();
    for file in &predecessor.manifest.files {
        keys.insert(format!("{root}/{}", file.path));
    }
    keys.insert(format!(
        "{root}/Axiusflow-Setup{}",
        std::env::consts::EXE_SUFFIX
    ));
    keys.insert(format!("{root}/provenance.json"));
    keys.insert(format!("{root}/{BLOCK_PLAN_FILENAME}"));
    keys.insert(format!("{root}/{RELEASE_RETIREMENT_FILENAME}"));
    keys.remove(&format!("{root}/manifest.json"));
    keys.into_iter().collect()
}

fn retire_predecessor_release(
    config: &PublisherConfig,
    bucket: &str,
    predecessor: &SignedReleaseManifest,
    verify_root: &Path,
) -> Result<(), String> {
    let manifest = &predecessor.manifest;
    validate_predecessor_manifest_shape(manifest)?;
    for object_key in predecessor_release_object_keys(predecessor) {
        remove_remote_object_and_verify_absent(config, bucket, &object_key, verify_root)?;
        verify_public_object_absent(config, &object_key)?;
    }
    let manifest_key = format!("{}/manifest.json", release_object_root(manifest));
    // Keep the signed predecessor manifest available until every other object
    // is absent. A failed cleanup can then be retried without guessing which
    // release the stable channel previously referenced.
    remove_remote_object_and_verify_absent(config, bucket, &manifest_key, verify_root)?;
    verify_public_object_absent(config, &manifest_key)
}

fn remove_remote_object_and_verify_absent(
    config: &PublisherConfig,
    bucket: &str,
    object_key: &str,
    verify_root: &Path,
) -> Result<(), String> {
    if let RemoteObject::Downloaded(path) = wrangler_get(config, bucket, object_key, verify_root)? {
        let _ = fs::remove_file(path);
        wrangler_delete(config, bucket, object_key)?;
    }
    match wrangler_get(config, bucket, object_key, verify_root)? {
        RemoteObject::Missing => Ok(()),
        RemoteObject::Downloaded(path) => {
            let _ = fs::remove_file(path);
            Err(format!(
                "retired R2 object remains after deletion: {object_key}"
            ))
        }
    }
}

fn verify_public_channel_pointer(
    config: &PublisherConfig,
    expected: &ReleaseChannelPointer,
) -> Result<(), String> {
    let url = format!(
        "{}/channels/{}/{}/{}.json",
        config.base_url, expected.channel, expected.platform, expected.architecture
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .max_idle_connections(1)
        .max_idle_connections_per_host(1)
        .timeout_global(Some(PUBLIC_VERIFY_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .call()
        .map_err(|_| "public stable channel verification request failed".to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAXIMUM_CHANNEL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "public stable channel verification read failed".to_string())?;
    if bytes.len() as u64 > MAXIMUM_CHANNEL_BYTES {
        return Err("public stable channel exceeds the size bound".to_string());
    }
    let actual: ReleaseChannelPointer = serde_json::from_slice(&bytes)
        .map_err(|_| "public stable channel is malformed".to_string())?;
    if &actual != expected {
        return Err("public stable channel does not match R2".to_string());
    }
    Ok(())
}

fn verify_current_installer_alias(
    config: &PublisherConfig,
    channel: &ReleaseChannelPointer,
) -> Result<(), String> {
    let url = format!(
        "{}/{}/{}/current/{}",
        config.base_url, channel.platform, channel.architecture, channel.installer.filename
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .max_idle_connections(1)
        .max_idle_connections_per_host(1)
        .timeout_global(Some(PUBLIC_VERIFY_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .call()
        .map_err(|_| "current installer alias verification request failed".to_string())?;
    if response.status().as_u16() != 200 {
        return Err("current installer alias did not return HTTP 200".to_string());
    }
    let cache_control = response
        .headers()
        .get("Cache-Control")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "current installer alias omitted Cache-Control".to_string())?;
    if !cache_control
        .split(',')
        .map(str::trim)
        .any(|token| token.eq_ignore_ascii_case("no-store"))
    {
        return Err("current installer alias must use Cache-Control: no-store".to_string());
    }
    let content_length = response
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "current installer alias omitted a valid Content-Length".to_string())?;
    if content_length != channel.installer.size {
        return Err("current installer alias Content-Length is stale".to_string());
    }
    let mut reader = response
        .body_mut()
        .as_reader()
        .take(channel.installer.size.saturating_add(1));
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "current installer alias could not be read".to_string())?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| "current installer alias size overflowed".to_string())?;
        digest.update(&buffer[..count]);
    }
    if total != channel.installer.size
        || URL_SAFE_NO_PAD.encode(digest.finalize()) != channel.installer.sha256_b64url
    {
        return Err(
            "current installer alias does not match the stable channel installer".to_string(),
        );
    }
    Ok(())
}

fn verify_public_object_absent(config: &PublisherConfig, object_key: &str) -> Result<(), String> {
    let Some(public_key) = object_key.strip_prefix("releases/") else {
        return Err("retired R2 object is outside the public release namespace".to_string());
    };
    let url = format!("{}/{public_key}", config.base_url);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .max_idle_connections(1)
        .max_idle_connections_per_host(1)
        .timeout_global(Some(PUBLIC_VERIFY_TIMEOUT))
        .build()
        .into();
    match agent.get(&url).call() {
        Err(ureq::Error::StatusCode(status)) if public_status_is_retired(status) => Ok(()),
        Ok(_) => Err(format!(
            "retired public release object remains available: {object_key}"
        )),
        Err(_) => Err(format!(
            "retired public release object absence could not be verified: {object_key}"
        )),
    }
}

fn public_status_is_retired(status: u16) -> bool {
    matches!(status, 404 | 410)
}

fn verify_block_plan_readback(
    path: &Path,
    predecessor: &ReleaseChannelPointer,
    target_manifest_path: &Path,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|_| "release block plan read-back failed".to_string())?;
    let target: SignedReleaseManifest = serde_json::from_slice(
        &fs::read(target_manifest_path)
            .map_err(|_| "candidate signed manifest could not be read".to_string())?,
    )
    .map_err(|_| "candidate signed manifest is malformed".to_string())?;
    decode_and_verify_block_plan(
        &bytes,
        &predecessor.signed_release.manifest,
        &target.manifest,
        verifying_key,
    )
    .map(|_| ())
    .map_err(|error| format!("release block plan read-back verification failed: {error}"))
}

fn verify_candidate_release_for_publication(
    release: &PublishedRelease,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let signed: SignedReleaseManifest = serde_json::from_slice(
        &fs::read(&release.manifest_path)
            .map_err(|_| "candidate signed manifest could not be read".to_string())?,
    )
    .map_err(|_| "candidate signed manifest is malformed".to_string())?;
    verify_release_manifest(&signed, verifying_key, &ReleasePolicy::native(0))
        .map_err(|_| "candidate signed manifest failed final verification".to_string())?;
    for file in &signed.manifest.files {
        verify_release_file(&release.release_directory.join(&file.path), file)
            .map_err(|_| "candidate release artifact failed final verification".to_string())?;
    }
    let provenance = release
        .immutable_objects
        .iter()
        .find(|object| object.requires_provenance_signature)
        .ok_or_else(|| "candidate signed provenance is missing".to_string())?;
    verify_release_provenance_file(&provenance.local_path, verifying_key)
}

fn build_optional_block_plan_object(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    predecessor: &ReleaseChannelPointer,
    signing_key: &SigningKey,
    verify_root: &Path,
) -> Result<Option<UploadObject>, String> {
    let target_signed: SignedReleaseManifest = serde_json::from_slice(
        &fs::read(&release.manifest_path)
            .map_err(|_| "candidate signed manifest could not be read".to_string())?,
    )
    .map_err(|_| "candidate signed manifest is malformed".to_string())?;
    let source = &predecessor.signed_release.manifest;
    let target = &target_signed.manifest;
    let mut plans = collect_block_file_plans(config, bucket, release, source, target, verify_root)?;
    if plans.is_empty() {
        return Ok(None);
    }
    plans.sort_by(|left, right| left.path.cmp(&right.path));
    let signed = sign_block_plan(
        BlockPlan {
            schema_version: BLOCK_PLAN_SCHEMA_VERSION,
            source_release_identity: source.release_identity.clone(),
            source_install_generation: source.install_generation,
            target_release_identity: target.release_identity.clone(),
            target_install_generation: target.install_generation,
            files: plans,
        },
        source,
        target,
        signing_key,
    )
    .map_err(|error| format!("release block plan signing failed: {error}"))?;
    let encoded = serde_json::to_vec(&signed)
        .map_err(|_| "release block plan could not be serialized".to_string())?;
    decode_and_verify_block_plan(&encoded, source, target, &signing_key.verifying_key())
        .map_err(|error| format!("release block plan verification failed: {error}"))?;
    let path = release.release_directory.join(BLOCK_PLAN_FILENAME);
    write_bytes_new_or_exact(&path, &encoded)?;
    Ok(Some(UploadObject {
        local_path: path,
        object_key: format!(
            "releases/{}/{}/{}-{}/{}",
            target.platform,
            target.architecture,
            target.install_generation,
            target.release_identity,
            BLOCK_PLAN_FILENAME
        ),
        content_type: "application/json",
        requires_authenticode: false,
        requires_provenance_signature: false,
    }))
}

fn collect_block_file_plans(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    source: &ReleaseManifest,
    target: &ReleaseManifest,
    verify_root: &Path,
) -> Result<Vec<BlockFilePlan>, String> {
    let mut plans = Vec::new();
    let mut total_blocks = 0_usize;
    let mut total_downloads = 0_usize;
    for target_file in &target.files {
        let Some(plan) = fetch_predecessor_block_file_plan(
            config,
            bucket,
            release,
            source,
            target_file,
            verify_root,
        )?
        else {
            continue;
        };
        let block_count = plan.blocks.len();
        let download_count = plan
            .blocks
            .iter()
            .filter(|block| block.source_offset.is_none())
            .count();
        if total_blocks.saturating_add(block_count) > MAXIMUM_BLOCK_PLAN_BLOCKS
            || total_downloads.saturating_add(download_count) > MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS
        {
            continue;
        }
        total_blocks += block_count;
        total_downloads += download_count;
        plans.push(plan);
    }
    Ok(plans)
}

fn fetch_predecessor_block_file_plan(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    source: &ReleaseManifest,
    target_file: &ReleaseFile,
    verify_root: &Path,
) -> Result<Option<BlockFilePlan>, String> {
    let Some(source_file) = source
        .files
        .iter()
        .find(|file| file.path == target_file.path)
    else {
        return Ok(None);
    };
    let expected_source_url = format!(
        "{}/{}/{}/{}-{}/{}",
        config.base_url,
        source.platform,
        source.architecture,
        source.install_generation,
        source.release_identity,
        target_file.path
    );
    if source_file.url != expected_source_url {
        return Ok(None);
    }
    let source_object_key = format!(
        "releases/{}/{}/{}-{}/{}",
        source.platform,
        source.architecture,
        source.install_generation,
        source.release_identity,
        target_file.path
    );
    let Ok(remote) = wrangler_get(config, bucket, &source_object_key, verify_root) else {
        return Ok(None);
    };
    let RemoteObject::Downloaded(source_path) = remote else {
        return Ok(None);
    };
    let target_path = release.release_directory.join(&target_file.path);
    let candidate = (|| {
        if verify_release_file(&source_path, source_file).is_err() {
            return Ok(None);
        }
        verify_release_file(&target_path, target_file)
            .map_err(|_| "candidate release artifact verification failed".to_string())?;
        build_block_file_plan(&source_path, source_file, &target_path, target_file)
    })();
    let _ = fs::remove_file(source_path);
    candidate
}

fn build_block_file_plan(
    source_path: &Path,
    source: &ReleaseFile,
    target_path: &Path,
    target: &ReleaseFile,
) -> Result<Option<BlockFilePlan>, String> {
    let source_blocks = source.size.div_ceil(BLOCK_PLAN_BLOCK_BYTES);
    let target_blocks = target.size.div_ceil(BLOCK_PLAN_BLOCK_BYTES);
    let maximum_blocks = u64::try_from(MAXIMUM_BLOCK_PLAN_BLOCKS)
        .map_err(|_| "release block count bound is invalid".to_string())?;
    if source_blocks == 0
        || target_blocks == 0
        || source_blocks > maximum_blocks
        || target_blocks > maximum_blocks
    {
        return Ok(None);
    }

    let mut source_file = File::open(source_path)
        .map_err(|_| "predecessor release artifact could not be opened".to_string())?;
    let block_bytes = usize::try_from(BLOCK_PLAN_BLOCK_BYTES)
        .map_err(|_| "release block size does not fit this platform".to_string())?;
    let mut buffer = vec![0_u8; block_bytes].into_boxed_slice();
    let mut source_index = BTreeMap::<(u32, [u8; 32]), u64>::new();
    let mut source_offset = 0_u64;
    while source_offset < source.size {
        let expected =
            usize::try_from((source.size - source_offset).min(BLOCK_PLAN_BLOCK_BYTES))
                .map_err(|_| "release source block size does not fit this platform".to_string())?;
        let read = read_exact_block(&mut source_file, &mut buffer[..expected])?;
        if read != expected {
            return Err("predecessor release artifact ended unexpectedly".to_string());
        }
        let read_u32 = u32::try_from(read)
            .map_err(|_| "release source block length overflowed".to_string())?;
        let read_u64 = u64::try_from(read)
            .map_err(|_| "release source block length overflowed".to_string())?;
        let digest: [u8; 32] = Sha256::digest(&buffer[..read]).into();
        source_index
            .entry((read_u32, digest))
            .or_insert(source_offset);
        source_offset = source_offset
            .checked_add(read_u64)
            .ok_or_else(|| "release source block offset overflowed".to_string())?;
    }

    let mut target_file = File::open(target_path)
        .map_err(|_| "candidate release artifact could not be opened".to_string())?;
    let mut target_offset = 0_u64;
    let target_capacity = usize::try_from(target_blocks)
        .map_err(|_| "release target block count does not fit this platform".to_string())?;
    let mut blocks = Vec::with_capacity(target_capacity);
    let mut reused = false;
    while target_offset < target.size {
        let expected =
            usize::try_from((target.size - target_offset).min(BLOCK_PLAN_BLOCK_BYTES))
                .map_err(|_| "release target block size does not fit this platform".to_string())?;
        let read = read_exact_block(&mut target_file, &mut buffer[..expected])?;
        if read != expected {
            return Err("candidate release artifact ended unexpectedly".to_string());
        }
        let read_u32 = u32::try_from(read)
            .map_err(|_| "release target block length overflowed".to_string())?;
        let read_u64 = u64::try_from(read)
            .map_err(|_| "release target block length overflowed".to_string())?;
        let digest: [u8; 32] = Sha256::digest(&buffer[..read]).into();
        let source_offset = source_index.get(&(read_u32, digest)).copied();
        reused |= source_offset.is_some();
        blocks.push(BlockDescriptor {
            length: read_u32,
            sha256: URL_SAFE_NO_PAD.encode(digest),
            source_offset,
        });
        target_offset = target_offset
            .checked_add(read_u64)
            .ok_or_else(|| "release target block offset overflowed".to_string())?;
    }
    if !reused {
        return Ok(None);
    }
    Ok(Some(BlockFilePlan {
        path: target.path.clone(),
        source_size: source.size,
        source_sha256: source.sha256.clone(),
        target_size: target.size,
        target_sha256: target.sha256.clone(),
        blocks,
    }))
}

fn read_exact_block(file: &mut File, buffer: &mut [u8]) -> Result<usize, String> {
    let mut total = 0_usize;
    while total < buffer.len() {
        let count = file
            .read(&mut buffer[total..])
            .map_err(|_| "release artifact block could not be read".to_string())?;
        if count == 0 {
            break;
        }
        total += count;
    }
    Ok(total)
}

fn write_bytes_new_or_exact(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "release metadata could not be committed".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path)
                .map_err(|_| "existing immutable release metadata could not be read".to_string())?;
            if existing == bytes {
                Ok(())
            } else {
                Err("immutable release metadata already exists with different content".to_string())
            }
        }
        Err(_) => Err("immutable release metadata could not be created".to_string()),
    }
}

fn ensure_wrangler_size(object: &UploadObject) -> Result<(), String> {
    let size = fs::metadata(&object.local_path)
        .map_err(|_| "release upload object metadata is unavailable".to_string())?
        .len();
    if size > WRANGLER_MAXIMUM_OBJECT_BYTES {
        return Err(format!(
            "{} exceeds Wrangler's 315 MiB single-object upload limit; use an R2 S3-compatible multipart client for this release",
            object.object_key
        ));
    }
    Ok(())
}

fn wrangler_put(
    config: &PublisherConfig,
    bucket: &str,
    object: &UploadObject,
    cache_control: &str,
) -> Result<(), String> {
    let target = format!("{bucket}/{}", object.object_key);
    let status = Command::new(&config.wrangler)
        .args(["r2", "object", "put"])
        .arg(target)
        .arg("--file")
        .arg(&object.local_path)
        .args([
            "--remote",
            "--content-type",
            object.content_type,
            "--cache-control",
            cache_control,
        ])
        .stdin(Stdio::null())
        .status()
        .map_err(|_| "Wrangler R2 upload could not be started".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Wrangler R2 upload failed for {}",
            object.object_key
        ))
    }
}

fn wrangler_delete(config: &PublisherConfig, bucket: &str, object_key: &str) -> Result<(), String> {
    let target = format!("{bucket}/{object_key}");
    let status = Command::new(&config.wrangler)
        .args(["r2", "object", "delete"])
        .arg(target)
        .arg("--remote")
        .stdin(Stdio::null())
        .status()
        .map_err(|_| "Wrangler R2 delete could not be started".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Wrangler R2 delete failed for {object_key}"))
    }
}

enum RemoteObject {
    Missing,
    Downloaded(PathBuf),
}

fn wrangler_get(
    config: &PublisherConfig,
    bucket: &str,
    object_key: &str,
    verify_root: &Path,
) -> Result<RemoteObject, String> {
    let suffix = URL_SAFE_NO_PAD.encode(Sha256::digest(object_key.as_bytes()));
    let extension = Path::new(object_key)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| valid_identifier(value, 16))
        .unwrap_or("object");
    let destination = verify_root.join(format!("{suffix}.{extension}"));
    let _ = fs::remove_file(&destination);
    let target = format!("{bucket}/{object_key}");
    let output = Command::new(&config.wrangler)
        .args(["r2", "object", "get"])
        .arg(target)
        .arg("--file")
        .arg(&destination)
        .arg("--remote")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "Wrangler R2 read could not be started".to_string())?;
    if output.status.success() {
        return Ok(RemoteObject::Downloaded(destination));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stderr.contains("The specified key does not exist.")
        || stdout.contains("The specified key does not exist.")
    {
        let _ = fs::remove_file(destination);
        return Ok(RemoteObject::Missing);
    }
    let _ = fs::remove_file(destination);
    Err(format!("Wrangler R2 read failed for {object_key}"))
}

fn verify_uploaded_object(
    config: &PublisherConfig,
    object: &UploadObject,
    downloaded: &Path,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let local_size = fs::metadata(&object.local_path)
        .map_err(|_| "local release object metadata is unavailable".to_string())?
        .len();
    let remote_size = fs::metadata(downloaded)
        .map_err(|_| "downloaded R2 verification object is unavailable".to_string())?
        .len();
    if local_size != remote_size
        || file_sha256_base64url(&object.local_path)? != file_sha256_base64url(downloaded)?
    {
        return Err(format!(
            "R2 read-back verification failed for {}",
            object.object_key
        ));
    }
    if object.requires_authenticode && cfg!(target_os = "windows") {
        verify_authenticode_file(config, downloaded)?;
    }
    if object.requires_provenance_signature {
        verify_release_provenance_file(downloaded, verifying_key)?;
    }
    Ok(())
}

fn verify_public_object(config: &PublisherConfig, object: &UploadObject) -> Result<(), String> {
    let public_key = if let Some(key) = object.object_key.strip_prefix("releases/") {
        key
    } else if object.object_key.starts_with("channels/") {
        object.object_key.as_str()
    } else {
        return Err("R2 object is outside the public release namespace".to_string());
    };
    let url = format!("{}/{public_key}", config.base_url);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .max_idle_connections(1)
        .max_idle_connections_per_host(1)
        .timeout_global(Some(PUBLIC_VERIFY_TIMEOUT))
        .build()
        .into();
    let expected_size = fs::metadata(&object.local_path)
        .map_err(|_| "local release object metadata is unavailable".to_string())?
        .len();
    let expected_hash = file_sha256_base64url(&object.local_path)?;
    let mut response = agent.get(&url).call().map_err(|_| {
        format!(
            "public release verification request failed for {}",
            object.object_key
        )
    })?;
    let mut reader = response.body_mut().as_reader().take(expected_size + 1);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer).map_err(|_| {
            format!(
                "public release verification read failed for {}",
                object.object_key
            )
        })?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| "public release verification size overflowed".to_string())?;
        digest.update(&buffer[..count]);
    }
    if total != expected_size || URL_SAFE_NO_PAD.encode(digest.finalize()) != expected_hash {
        return Err(format!(
            "public release verification failed for {}",
            object.object_key
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_platform_runtime::{
        ReleasePolicy, SignedReleaseManifest, verify_release_manifest,
    };

    fn temporary_root(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!("release-publisher-{name}-{nanos}"));
        fs::create_dir_all(&root).expect("temporary publisher root");
        root
    }

    fn previous_compatible_launcher_policy() -> ReleasePolicy {
        ReleasePolicy {
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            current_version: "0.2.0".to_string(),
            minimum_install_generation: 18,
            maximum_release_bytes: u64::MAX,
        }
    }

    fn publisher_config_fixture(root: &Path, generation: u64) -> PublisherConfig {
        PublisherConfig {
            signing_key_file: root.join("unused"),
            expected_verifying_key: None,
            skip_build: false,
            release_identity: "0123456789abcdef0123456789abcdef01234567".to_string(),
            generation,
            release_version: "0.2.2".to_string(),
            minimum_version: "0.2.0".to_string(),
            base_url: "https://auth.axiusflow.test/releases".to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            channel: "stable".to_string(),
            rollout_cohort: "stable".to_string(),
            rollout_percentage: 25,
            output_root: root.join("out"),
            r2_bucket: None,
            wrangler: OsString::from("wrangler"),
            iscc: OsString::from("ISCC.exe"),
            authenticode_tool: OsString::from("signtool.exe"),
            authenticode_certificate_sha1: Some(
                "0123456789abcdef0123456789abcdef01234567".to_string(),
            ),
            authenticode_timestamp_url: Some("https://timestamp.example.test".to_string()),
        }
    }

    fn release_file_fixture(path: &str, bytes: &[u8]) -> ReleaseFile {
        ReleaseFile {
            role: ReleaseFileRole::Desktop,
            path: path.to_string(),
            url: format!("https://releases.axiusflow.test/{path}"),
            size: bytes.len() as u64,
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            executable: true,
        }
    }

    fn channel_fixture(
        config: &PublisherConfig,
        key: &SigningKey,
        generation: u64,
        identity: &str,
    ) -> ReleaseChannelPointer {
        let file_path = format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX);
        let file = ReleaseFile {
            url: format!(
                "{}/{}/{}/{}-{identity}/{file_path}",
                config.base_url,
                std::env::consts::OS,
                std::env::consts::ARCH,
                generation
            ),
            ..release_file_fixture(&file_path, b"desktop")
        };
        let signed_release = sign_release_manifest(
            ReleaseManifest {
                schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
                release_identity: identity.to_string(),
                install_generation: generation,
                channel: config.channel.clone(),
                minimum_version: config.minimum_version.clone(),
                platform: std::env::consts::OS.to_string(),
                architecture: std::env::consts::ARCH.to_string(),
                files: vec![file],
                rollout: RolloutMetadata {
                    cohort: config.rollout_cohort.clone(),
                    percentage: config.rollout_percentage,
                },
            },
            key,
        )
        .expect("release signs");
        let installer_name = format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX);
        ReleaseChannelPointer {
            schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
            channel: config.channel.clone(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            release_identity: identity.to_string(),
            install_generation: generation,
            version: config.release_version.clone(),
            published_at: config.published_at.clone(),
            manifest_url: format!(
                "{}/{}/{}/{}-{identity}/manifest.json",
                config.base_url,
                std::env::consts::OS,
                std::env::consts::ARCH,
                generation
            ),
            signed_release,
            installer: ReleaseInstallerMetadata {
                filename: installer_name.clone(),
                url: format!(
                    "{}/{}/{}/{}-{identity}/{installer_name}",
                    config.base_url,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    generation
                ),
                size: 1,
                sha256_b64url: URL_SAFE_NO_PAD.encode([1_u8; 32]),
            },
        }
    }

    #[test]
    fn release_retry_contracts_cover_partial_upload_and_post_channel_cleanup() {
        let root = temporary_root("release-retry");
        let config = publisher_config_fixture(&root, 8);
        let key = SigningKey::from_bytes(&[10; 32]);
        let current = channel_fixture(&config, &key, 8, &config.release_identity);
        validate_predecessor_channel(&config, &current, &key.verifying_key())
            .expect("current channel validates");
        assert!(matches!(
            classify_remote_channel_progression(&config, current.clone(), &current)
                .expect("same generation is retryable"),
            RemoteChannelProgression::Current(_)
        ));
        let mut different_candidate = current.clone();
        different_candidate.installer.size += 1;
        assert!(matches!(
            classify_remote_channel_progression(&config, current.clone(), &different_candidate)
                .expect("committed generation remains identifiable"),
            RemoteChannelProgression::CommittedGenerationMismatch(_)
        ));

        let predecessor =
            channel_fixture(&config, &key, 7, "fedcba9876543210fedcba9876543210fedcba98");
        let signed_retirement = sign_release_retirement(
            ReleaseRetirement {
                schema_version: RELEASE_RETIREMENT_SCHEMA_VERSION,
                target_release_identity: current.release_identity.clone(),
                target_install_generation: current.install_generation,
                predecessor: Some(predecessor.signed_release.clone()),
            },
            &key,
        )
        .expect("retirement signs");
        verify_release_retirement(
            &signed_retirement,
            &current.signed_release.manifest,
            &key.verifying_key(),
        )
        .expect("post-channel retry has an authenticated predecessor");
        let retired_keys = predecessor_release_object_keys(&predecessor.signed_release);
        assert!(
            retired_keys
                .iter()
                .any(|key| key.ends_with("provenance.json"))
        );
        assert!(
            retired_keys
                .iter()
                .any(|key| key.ends_with(BLOCK_PLAN_FILENAME))
        );
        assert!(
            retired_keys
                .iter()
                .any(|key| key.ends_with(RELEASE_RETIREMENT_FILENAME))
        );
        assert!(
            !retired_keys
                .iter()
                .any(|key| key.ends_with("manifest.json"))
        );

        let local = root.join("candidate.json");
        let downloaded = root.join("downloaded.json");
        fs::write(&local, b"candidate").expect("local candidate");
        fs::write(&downloaded, b"candidate").expect("downloaded candidate");
        let object = UploadObject {
            local_path: local,
            object_key: "releases/windows/x86_64/8-test/candidate.json".to_string(),
            content_type: "application/json",
            requires_authenticode: false,
            requires_provenance_signature: false,
        };
        verify_uploaded_object(&config, &object, &downloaded, &key.verifying_key())
            .expect("exact partial upload is reusable");
        fs::write(&downloaded, b"different").expect("mismatched remote candidate");
        assert!(
            verify_uploaded_object(&config, &object, &downloaded, &key.verifying_key()).is_err()
        );

        assert!(public_status_is_retired(404));
        assert!(public_status_is_retired(410));
        assert!(!public_status_is_retired(200));
        let sidecar = root.join("retry-sidecar.json");
        write_bytes_new_or_exact(&sidecar, b"same").expect("first sidecar write");
        write_bytes_new_or_exact(&sidecar, b"same").expect("exact retry reuses sidecar");
        assert!(write_bytes_new_or_exact(&sidecar, b"different").is_err());
        fs::remove_dir_all(root).expect("remove retry fixture");
    }

    #[test]
    fn predecessor_channel_cross_binding_and_manifest_shape_fail_closed() {
        let root = temporary_root("predecessor-validation");
        let config = publisher_config_fixture(&root, 8);
        let key = SigningKey::from_bytes(&[11; 32]);
        let identity = "fedcba9876543210fedcba9876543210fedcba98";
        let file = ReleaseFile {
            url: format!(
                "{}/{}/{}/7-{identity}/app.bin",
                config.base_url,
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            ..release_file_fixture("app.bin", b"source")
        };
        let manifest = ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: identity.to_string(),
            install_generation: 7,
            channel: "stable".to_string(),
            minimum_version: "0.2.0".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            files: vec![file],
            rollout: RolloutMetadata {
                cohort: "all".to_string(),
                percentage: 100,
            },
        };
        let signed_release = sign_release_manifest(manifest, &key).expect("source signs");
        let mut channel = ReleaseChannelPointer {
            schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
            channel: "stable".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            release_identity: identity.to_string(),
            install_generation: 7,
            version: "0.2.0".to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            manifest_url: format!(
                "{}/{}/{}/7-{identity}/manifest.json",
                config.base_url,
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            signed_release,
            installer: ReleaseInstallerMetadata {
                filename: format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX),
                url: format!(
                    "{}/{}/{}/7-{identity}/Axiusflow-Setup{}",
                    config.base_url,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    std::env::consts::EXE_SUFFIX
                ),
                size: 1,
                sha256_b64url: URL_SAFE_NO_PAD.encode([1_u8; 32]),
            },
        };
        validate_predecessor_channel(&config, &channel, &key.verifying_key())
            .expect("valid predecessor accepted");

        // A cryptographically authentic predecessor can use a retired release
        // inventory shape. It is never installed by this publisher; it is only
        // a bounded, cross-bound block-reuse source. Keep accepting such a
        // predecessor after current candidate policy retires the old role.
        let mut legacy = channel.clone();
        let engine_path = "axiusflow_engine.exe";
        legacy.signed_release.manifest.files.push(ReleaseFile {
            role: ReleaseFileRole::Engine,
            path: engine_path.to_string(),
            url: format!(
                "{}/{}/{}/7-{identity}/{engine_path}",
                config.base_url,
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            size: 6,
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(b"engine")),
            executable: true,
        });
        let canonical =
            serde_json::to_vec(&legacy.signed_release.manifest).expect("legacy manifest JSON");
        legacy.signed_release.signature = URL_SAFE_NO_PAD.encode(key.sign(&canonical).to_bytes());
        validate_predecessor_channel(&config, &legacy, &key.verifying_key())
            .expect("signed retired predecessor shape remains an authenticated block source");

        channel.platform = "other".to_string();
        assert!(validate_predecessor_channel(&config, &channel, &key.verifying_key()).is_err());
        channel.platform = std::env::consts::OS.to_string();
        channel.signed_release.manifest.rollout.percentage = 101;
        let canonical =
            serde_json::to_vec(&channel.signed_release.manifest).expect("manifest JSON");
        channel.signed_release.signature = URL_SAFE_NO_PAD.encode(key.sign(&canonical).to_bytes());
        assert!(validate_predecessor_channel(&config, &channel, &key.verifying_key()).is_err());
        fs::remove_dir_all(root).expect("remove predecessor fixture");
    }

    #[test]
    fn final_candidate_audit_rejects_post_package_artifact_mutation() {
        let root = temporary_root("final-candidate-audit");
        let binaries_root = root.join("binaries");
        fs::create_dir_all(&binaries_root).expect("binaries root");
        let suffix = std::env::consts::EXE_SUFFIX;
        let binaries = ReleaseBinaries {
            launcher: binaries_root.join(format!("launcher{suffix}")),
            desktop: binaries_root.join(format!("desktop{suffix}")),
        };
        fs::write(&binaries.launcher, b"launcher").expect("launcher fixture");
        fs::write(&binaries.desktop, b"desktop").expect("desktop fixture");
        let config = publisher_config_fixture(&root, 41);
        let key = SigningKey::from_bytes(&[12; 32]);
        let published = package_release(&root, &config, &key, &binaries).expect("package release");
        verify_candidate_release_for_publication(&published, &key.verifying_key())
            .expect("fresh package audits");
        let signed: SignedReleaseManifest =
            serde_json::from_slice(&fs::read(&published.manifest_path).expect("manifest bytes"))
                .expect("signed manifest");
        fs::write(
            published
                .release_directory
                .join(&signed.manifest.files[0].path),
            b"tampered",
        )
        .expect("tamper packaged artifact");
        assert!(
            verify_candidate_release_for_publication(&published, &key.verifying_key()).is_err()
        );
        fs::remove_dir_all(root).expect("remove final-audit fixture");
    }

    #[test]
    fn block_plan_readback_reverifies_signature_and_release_binding() {
        let root = temporary_root("block-plan-readback");
        let key = SigningKey::from_bytes(&[13; 32]);
        let payload = b"shared block";
        let make_manifest = |identity: &str, generation: u64| ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: identity.to_string(),
            install_generation: generation,
            channel: "stable".to_string(),
            minimum_version: "0.2.0".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            files: vec![release_file_fixture("app.bin", payload)],
            rollout: RolloutMetadata {
                cohort: "all".to_string(),
                percentage: 100,
            },
        };
        let source = sign_release_manifest(
            make_manifest("1111111111111111111111111111111111111111", 7),
            &key,
        )
        .expect("source signs");
        let target = sign_release_manifest(
            make_manifest("2222222222222222222222222222222222222222", 8),
            &key,
        )
        .expect("target signs");
        let file = &target.manifest.files[0];
        let signed_plan = sign_block_plan(
            BlockPlan {
                schema_version: BLOCK_PLAN_SCHEMA_VERSION,
                source_release_identity: source.manifest.release_identity.clone(),
                source_install_generation: 7,
                target_release_identity: target.manifest.release_identity.clone(),
                target_install_generation: 8,
                files: vec![BlockFilePlan {
                    path: file.path.clone(),
                    source_size: source.manifest.files[0].size,
                    source_sha256: source.manifest.files[0].sha256.clone(),
                    target_size: file.size,
                    target_sha256: file.sha256.clone(),
                    blocks: vec![BlockDescriptor {
                        length: u32::try_from(payload.len()).expect("payload fits u32"),
                        sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(payload)),
                        source_offset: Some(0),
                    }],
                }],
            },
            &source.manifest,
            &target.manifest,
            &key,
        )
        .expect("block plan signs");
        let sidecar = root.join(BLOCK_PLAN_FILENAME);
        let target_path = root.join("manifest.json");
        fs::write(
            &sidecar,
            serde_json::to_vec(&signed_plan).expect("plan JSON"),
        )
        .expect("sidecar writes");
        fs::write(
            &target_path,
            serde_json::to_vec(&target).expect("target JSON"),
        )
        .expect("target manifest writes");
        let predecessor = ReleaseChannelPointer {
            schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
            channel: "stable".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            release_identity: source.manifest.release_identity.clone(),
            install_generation: 7,
            version: "0.2.0".to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            manifest_url: "https://releases.axiusflow.test/manifest.json".to_string(),
            signed_release: source,
            installer: ReleaseInstallerMetadata {
                filename: "setup.exe".to_string(),
                url: "https://releases.axiusflow.test/setup.exe".to_string(),
                size: 1,
                sha256_b64url: URL_SAFE_NO_PAD.encode([1_u8; 32]),
            },
        };
        verify_block_plan_readback(&sidecar, &predecessor, &target_path, &key.verifying_key())
            .expect("read-back verifies");
        fs::write(&sidecar, b"{}").expect("tamper sidecar");
        assert!(
            verify_block_plan_readback(&sidecar, &predecessor, &target_path, &key.verifying_key())
                .is_err()
        );
        fs::remove_dir_all(root).expect("remove readback fixture");
    }

    #[test]
    fn block_plan_reuses_moved_aligned_blocks_and_marks_changed_blocks_for_range_fetch() {
        let root = temporary_root("block-plan-selection");
        let block = usize::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits usize");
        let source_bytes = [vec![1_u8; block], vec![2_u8; block]].concat();
        let target_bytes = [vec![2_u8; block], vec![1_u8; block], vec![3_u8; block]].concat();
        let source_path = root.join("source.bin");
        let target_path = root.join("target.bin");
        fs::write(&source_path, &source_bytes).expect("source fixture");
        fs::write(&target_path, &target_bytes).expect("target fixture");
        let source = release_file_fixture("app.bin", &source_bytes);
        let target = release_file_fixture("app.bin", &target_bytes);

        let plan = build_block_file_plan(&source_path, &source, &target_path, &target)
            .expect("block selection succeeds")
            .expect("reuse is beneficial");
        assert_eq!(plan.blocks.len(), 3);
        assert_eq!(plan.blocks[0].source_offset, Some(BLOCK_PLAN_BLOCK_BYTES));
        assert_eq!(plan.blocks[1].source_offset, Some(0));
        assert_eq!(plan.blocks[2].source_offset, None);
        assert_eq!(plan.target_sha256, target.sha256);
        fs::remove_dir_all(root).expect("remove block-plan fixture");
    }

    #[test]
    fn block_plan_is_omitted_when_no_target_block_matches_the_predecessor() {
        let root = temporary_root("block-plan-no-reuse");
        let block = usize::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits usize");
        let source_bytes = vec![4_u8; block];
        let target_bytes = vec![5_u8; block];
        let source_path = root.join("source.bin");
        let target_path = root.join("target.bin");
        fs::write(&source_path, &source_bytes).expect("source fixture");
        fs::write(&target_path, &target_bytes).expect("target fixture");
        let source = release_file_fixture("app.bin", &source_bytes);
        let target = release_file_fixture("app.bin", &target_bytes);
        assert!(
            build_block_file_plan(&source_path, &source, &target_path, &target)
                .expect("block selection succeeds")
                .is_none()
        );
        fs::remove_dir_all(root).expect("remove block-plan fixture");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn package_emits_signed_manifest_with_versioned_launcher_and_matching_channel() {
        let root = temporary_root("package");
        let binaries_root = root.join("binaries");
        fs::create_dir_all(&binaries_root).expect("fixture binaries root");
        let suffix = std::env::consts::EXE_SUFFIX;
        let binaries = ReleaseBinaries {
            launcher: binaries_root.join(format!("launcher{suffix}")),
            desktop: binaries_root.join(format!("desktop{suffix}")),
        };
        fs::write(&binaries.launcher, b"launcher-fixture").expect("launcher fixture");
        fs::write(&binaries.desktop, b"desktop-fixture").expect("desktop fixture");
        let config = PublisherConfig {
            signing_key_file: root.join("unused"),
            expected_verifying_key: None,
            skip_build: false,
            release_identity: "0123456789abcdef0123456789abcdef01234567".to_string(),
            generation: 41,
            release_version: "7.8.9".to_string(),
            minimum_version: "0.2.0".to_string(),
            base_url: "https://auth.axiusflow.test/releases".to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            channel: "stable".to_string(),
            rollout_cohort: "stable".to_string(),
            rollout_percentage: 25,
            output_root: root.join("out"),
            r2_bucket: None,
            wrangler: OsString::from("wrangler"),
            iscc: OsString::from("ISCC.exe"),
            authenticode_tool: OsString::from("signtool.exe"),
            authenticode_certificate_sha1: Some(
                "0123456789abcdef0123456789abcdef01234567".to_string(),
            ),
            authenticode_timestamp_url: Some("https://timestamp.example.test".to_string()),
        };
        let key = SigningKey::from_bytes(&[7; 32]);
        let published = package_release(&root, &config, &key, &binaries).expect("package release");
        let signed: SignedReleaseManifest = serde_json::from_slice(
            &fs::read(&published.manifest_path).expect("signed manifest bytes"),
        )
        .expect("signed manifest");
        assert_eq!(signed.manifest.minimum_version, "0.2.0");
        assert_eq!(signed.manifest.rollout.cohort, "stable");
        assert_eq!(signed.manifest.rollout.percentage, 25);
        verify_release_manifest(
            &signed,
            &key.verifying_key(),
            &previous_compatible_launcher_policy(),
        )
        .expect("previous compatible launcher accepts the signed release");
        assert_eq!(signed.manifest.files.len(), 3);
        assert!(
            signed
                .manifest
                .files
                .iter()
                .all(|file| !file.path.contains("Setup"))
        );
        assert!(signed.manifest.files.iter().any(|file| {
            file.role == ReleaseFileRole::RuntimeAsset
                && file.path == format!("axiusflow_launcher{suffix}")
        }));
        assert!(signed.manifest.files.iter().any(|file| {
            file.role == ReleaseFileRole::RuntimeAsset
                && file.path == "rollback-compatibility.json"
                && !file.executable
        }));
        let channel: ReleaseChannelPointer =
            serde_json::from_slice(&fs::read(&published.channel_path).expect("channel bytes"))
                .expect("channel pointer");
        assert_eq!(channel.signed_release, signed);
        assert_eq!(
            channel.release_identity,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(channel.install_generation, 41);
        assert_eq!(
            channel.manifest_url,
            format!(
                "https://auth.axiusflow.test/releases/{}/{}/41-0123456789abcdef0123456789abcdef01234567/manifest.json",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
        assert_eq!(channel.version, "7.8.9");
        assert_eq!(channel.published_at, "2026-09-07T00:00:00Z");
        assert!(channel.installer.url.contains("/41-0123456789abcdef"));
        assert!(!channel.installer.url.contains("/releases/releases/"));
        assert_eq!(
            published.channel_object.object_key,
            format!(
                "channels/stable/{}/{}.json",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
        assert!(
            published
                .immutable_objects
                .iter()
                .any(|object| object.object_key.contains("Axiusflow-Setup"))
        );
        assert!(published.immutable_objects.iter().any(|object| {
            object
                .object_key
                .ends_with(&format!("axiusflow_launcher{suffix}"))
        }));
        let provenance_object = published
            .immutable_objects
            .iter()
            .find(|object| object.object_key.ends_with("/provenance.json"))
            .expect("signed provenance object");
        assert!(provenance_object.requires_provenance_signature);
        verify_release_provenance_file(&provenance_object.local_path, &key.verifying_key())
            .expect("signed provenance verifies");
        let provenance: SignedReleaseProvenance = serde_json::from_slice(
            &fs::read(&provenance_object.local_path).expect("provenance bytes"),
        )
        .expect("signed provenance JSON");
        assert_eq!(
            provenance.provenance.release_identity,
            config.release_identity
        );
        assert_eq!(provenance.provenance.install_generation, 41);
        assert_eq!(provenance.provenance.rollout.cohort, "stable");
        assert_eq!(provenance.provenance.rollout.percentage, 25);
        assert_eq!(provenance.provenance.artifacts.len(), 5);
        for name in [
            format!("Axiusflow-Setup{suffix}"),
            "manifest.json".to_string(),
            format!("axiusflow_launcher{suffix}"),
            format!("axiusflow_desktop{suffix}"),
            "rollback-compatibility.json".to_string(),
        ] {
            assert!(
                provenance
                    .provenance
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.name == name)
            );
        }
        for artifact in &provenance.provenance.artifacts {
            let path = published.release_directory.join(&artifact.name);
            assert_eq!(
                artifact.size,
                fs::metadata(&path)
                    .expect("provenance artifact metadata")
                    .len()
            );
            assert_eq!(
                artifact.sha256_b64url,
                file_sha256_base64url(&path).expect("provenance artifact hash")
            );
        }
        let wire: serde_json::Value =
            serde_json::from_slice(&fs::read(&published.channel_path).expect("channel wire bytes"))
                .expect("channel wire JSON");
        assert_eq!(wire["generation"], 41);
        assert_eq!(wire["arch"], std::env::consts::ARCH);
        assert!(wire.get("install_generation").is_none());
        assert_eq!(wire["signed_release"]["manifest"]["install_generation"], 41);
        assert_eq!(
            wire["signed_release"]["manifest"]["architecture"],
            std::env::consts::ARCH
        );
        fs::remove_dir_all(root).expect("remove publisher fixture");
    }

    #[test]
    fn base_url_and_identifiers_fail_closed() {
        assert!(normalize_base_url("http://releases.axiusflow.test").is_err());
        assert!(normalize_base_url("https://releases.axiusflow.test/path?x=1").is_err());
        assert!(valid_identifier("stable", 32));
        assert!(!valid_identifier("stable/channel", 32));
        assert!(valid_release_identity("0123456789abcdef"));
        assert!(!valid_release_identity("release-test"));
        assert!(valid_published_at("2026-09-07T00:00:00Z"));
        assert!(valid_published_at("2024-02-29T23:59:59.123Z"));
        assert!(!valid_published_at("2026-02-30T00:00:00Z"));
        assert!(!valid_published_at("2026-09-07T25:00:00Z"));
        assert!(valid_certificate_sha1(
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!valid_certificate_sha1("01234567"));
        assert!(valid_timestamp_url("https://timestamp.example.test"));
        assert!(valid_timestamp_url("http://timestamp.example.test/rfc3161"));
        assert!(!valid_timestamp_url("file:///timestamp"));
        assert!(!valid_timestamp_url(
            "https://timestamp.example.test/#fragment"
        ));
    }

    #[test]
    fn production_publication_requires_self_hosted_release_context() {
        assert!(verify_production_publication_context(None, |_| None).is_ok());
        assert!(
            verify_production_publication_context(Some("axiusflow-releases"), |_| None).is_err()
        );
        let expected = [
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_WORKFLOW", "Production release"),
            ("GITHUB_EVENT_NAME", "workflow_dispatch"),
            ("GITHUB_REF", "refs/heads/main"),
            (
                "AXIUSFLOW_RELEASE_ENVIRONMENT",
                "self-hosted-release-station",
            ),
        ];
        assert!(
            verify_production_publication_context(Some("axiusflow-releases"), |name| {
                expected
                    .iter()
                    .find(|(candidate, _)| *candidate == name)
                    .map(|(_, value)| OsString::from(value))
            })
            .is_ok()
        );
    }

    #[test]
    fn publisher_rollout_percentage_accepts_hold_and_rejects_out_of_range() {
        let arguments = |percentage: &str| {
            vec![
                OsString::from("--signing-key-file"),
                OsString::from("unused-key"),
                OsString::from("--release-identity"),
                OsString::from("0123456789abcdef0123456789abcdef01234567"),
                OsString::from("--generation"),
                OsString::from("42"),
                OsString::from("--release-version"),
                OsString::from("0.2.2"),
                OsString::from("--minimum-version"),
                OsString::from("0.2.0"),
                OsString::from("--base-url"),
                OsString::from("https://auth.axiusflow.test/releases"),
                OsString::from("--published-at"),
                OsString::from("2026-09-07T00:00:00Z"),
                OsString::from("--rollout-cohort"),
                OsString::from("stable"),
                OsString::from("--rollout-percentage"),
                OsString::from(percentage),
                OsString::from("--authenticode-certificate-sha1"),
                OsString::from("0123456789abcdef0123456789abcdef01234567"),
                OsString::from("--authenticode-timestamp-url"),
                OsString::from("https://timestamp.example.test"),
            ]
        };
        let held = PublisherConfig::parse(arguments("0").into_iter()).expect("0 percent hold");
        assert_eq!(held.rollout_cohort, "stable");
        assert_eq!(held.rollout_percentage, 0);
        assert!(PublisherConfig::parse(arguments("100").into_iter()).is_ok());
        assert!(PublisherConfig::parse(arguments("101").into_iter()).is_err());
    }

    #[test]
    fn provenance_signature_is_domain_separated_and_rejects_tampering() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let provenance = ReleaseProvenance {
            schema_version: RELEASE_PROVENANCE_SCHEMA_VERSION,
            release_identity: "0123456789abcdef0123456789abcdef01234567".to_string(),
            install_generation: 42,
            channel: "stable".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            rollout: RolloutMetadata {
                cohort: "stable".to_string(),
                percentage: 50,
            },
            artifacts: vec![
                ReleaseProvenanceArtifact {
                    name: format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX),
                    size: 1,
                    sha256_b64url: URL_SAFE_NO_PAD.encode([1_u8; 32]),
                },
                ReleaseProvenanceArtifact {
                    name: "manifest.json".to_string(),
                    size: 2,
                    sha256_b64url: URL_SAFE_NO_PAD.encode([2_u8; 32]),
                },
                ReleaseProvenanceArtifact {
                    name: format!("axiusflow_launcher{}", std::env::consts::EXE_SUFFIX),
                    size: 3,
                    sha256_b64url: URL_SAFE_NO_PAD.encode([3_u8; 32]),
                },
                ReleaseProvenanceArtifact {
                    name: format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX),
                    size: 4,
                    sha256_b64url: URL_SAFE_NO_PAD.encode([4_u8; 32]),
                },
                ReleaseProvenanceArtifact {
                    name: "rollback-compatibility.json".to_string(),
                    size: 5,
                    sha256_b64url: URL_SAFE_NO_PAD.encode([5_u8; 32]),
                },
            ],
        };
        let signed = sign_release_provenance(provenance.clone(), &key).expect("signed provenance");
        let canonical = serde_json::to_vec(&provenance).expect("canonical provenance");
        let signature = URL_SAFE_NO_PAD
            .decode(&signed.signature_b64url)
            .expect("signature bytes");
        let signature = Signature::from_slice(&signature).expect("signature");
        assert!(key.verifying_key().verify(&canonical, &signature).is_err());

        let root = temporary_root("provenance-tamper");
        let path = root.join("provenance.json");
        write_json_new(&path, &signed).expect("write signed provenance");
        verify_release_provenance_file(&path, &key.verifying_key()).expect("valid provenance");
        let mut tampered = signed;
        tampered.provenance.install_generation += 1;
        fs::write(
            &path,
            serde_json::to_vec_pretty(&tampered).expect("tampered JSON"),
        )
        .expect("write tamper");
        assert!(verify_release_provenance_file(&path, &key.verifying_key()).is_err());
        fs::remove_dir_all(root).expect("remove provenance fixture");
    }

    #[test]
    fn windows_authenticode_contract_is_rfc3161_sha256_and_verifies_timestamp_metadata() {
        let source = include_str!("axiusflow_release_publisher.rs");
        for required in [
            "\"/sha1\"",
            "\"/fd\"",
            "\"SHA256\"",
            "\"/tr\"",
            "\"/td\"",
            "Get-AuthenticodeSignature",
            "TimeStamperCertificate",
            "SignerCertificate.Thumbprint",
            "verify_public_object(config, &release.channel_object)?",
        ] {
            assert!(source.contains(required), "publisher lost {required}");
        }
        let launcher_sign = source
            .find("sign_authenticode_file(config, &launcher_path)?;")
            .expect("launcher signing hook");
        let desktop_sign = source
            .find("sign_authenticode_file(config, &desktop_path)?;")
            .expect("desktop signing hook");
        let manifest_inventory = source
            .find("let mut files = vec![")
            .expect("manifest inventory");
        let setup_sign = source
            .find("sign_authenticode_file(config, &setup_path)?;")
            .expect("setup signing hook");
        let setup_hash = source
            .find("let setup_metadata = fs::metadata(&setup_path)")
            .expect("setup metadata/hash boundary");
        assert!(launcher_sign < manifest_inventory);
        assert!(desktop_sign < manifest_inventory);
        assert!(setup_sign < setup_hash);
    }

    #[test]
    fn windows_installer_script_keeps_standard_registration_and_signed_install_boundary() {
        let script = include_str!("../../../../tools/windows/axiusflow_setup.iss");
        for required in [
            "PrivilegesRequired=lowest",
            "DefaultDirName={localappdata}\\Programs\\Axiusflow",
            "UninstallFilesDir={localappdata}\\Programs\\Axiusflow-Uninstall",
            "SetupIconFile={#IconPath}",
            "UninstallDisplayIcon={app}\\axiusflow_launcher.exe",
            "[Icons]",
            "procedure RegisterExtraCloseApplicationsResources;",
            "RegisterExtraCloseApplicationsResource",
            "function PrepareToInstall(var NeedsRestart: Boolean): String;",
            "{localappdata}\\Programs\\.Axiusflow-lifecycle\\uninstall.json",
            "Finishing the previous Axiusflow uninstall...",
            "Setup has not replaced the recovery launcher",
            "procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);",
            "--remove-all-local-data",
            "ewWaitUntilTerminated",
            "ResultCode <> 0",
            "Uninstall has stopped so cleanup can be retried safely.",
            "Abort;",
            "--install \"' + Manifest + '\" \"' + Bundle + '\"",
            "RollbackCompatibilityPath",
            "DestName: \"rollback-compatibility.json\"",
        ] {
            assert!(
                script.contains(required),
                "installer script lost {required}"
            );
        }
        assert!(!script.contains("[UninstallRun]"));
        assert!(!script.contains("Parameters: \"--launch-desktop\""));
    }
}
