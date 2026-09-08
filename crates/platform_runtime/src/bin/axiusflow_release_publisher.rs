//! Local, offline-key release packager and optional Wrangler R2 publisher.
//!
//! The signing key is read only by this process from an explicit local file.
//! Child Cargo/Wrangler processes receive the derived public key, never the
//! private key bytes or path through an environment variable.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use axiusflow_platform_runtime::{
    RELEASE_CHANNEL_SCHEMA_VERSION, RELEASE_MANIFEST_SCHEMA_VERSION, ReleaseChannelPointer,
    ReleaseFile, ReleaseFileRole, ReleaseInstallerMetadata, ReleaseManifest, RolloutMetadata,
    sign_release_manifest, verify_release_manifest_signature,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest as _, Sha256};

const DEFAULT_CHANNEL: &str = "stable";
const DEFAULT_OUTPUT_ROOT: &str = "target/release-publish";
const WRANGLER_MAXIMUM_OBJECT_BYTES: u64 = 315 * 1024 * 1024;
const MAXIMUM_CHANNEL_BYTES: u64 = 1024 * 1024;
const PUBLIC_VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("Axiusflow release publisher: {error}");
        std::process::exit(1);
    }
}

#[derive(Debug)]
struct PublisherConfig {
    signing_key_file: PathBuf,
    release_identity: String,
    generation: u64,
    base_url: String,
    published_at: String,
    channel: String,
    output_root: PathBuf,
    r2_bucket: Option<String>,
    wrangler: OsString,
    iscc: OsString,
}

impl PublisherConfig {
    fn parse(arguments: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut signing_key_file = None;
        let mut release_identity = None;
        let mut generation = None;
        let mut base_url = None;
        let mut published_at = None;
        let mut channel = DEFAULT_CHANNEL.to_string();
        let mut output_root = PathBuf::from(DEFAULT_OUTPUT_ROOT);
        let mut r2_bucket = None;
        let mut wrangler = OsString::from("wrangler");
        let mut iscc = OsString::from("ISCC.exe");
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
                "--base-url" => base_url = Some(required_argument(&mut arguments, &flag)?),
                "--published-at" => published_at = Some(required_argument(&mut arguments, &flag)?),
                "--channel" => channel = required_argument(&mut arguments, &flag)?,
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
                _ => return Err(usage()),
            }
        }
        let config = Self {
            signing_key_file: signing_key_file.ok_or_else(usage)?,
            release_identity: release_identity.ok_or_else(usage)?,
            generation: generation.ok_or_else(usage)?,
            base_url: normalize_base_url(&base_url.ok_or_else(usage)?)?,
            published_at: published_at.ok_or_else(usage)?,
            channel,
            output_root,
            r2_bucket,
            wrangler,
            iscc,
        };
        if !valid_release_identity(&config.release_identity)
            || !valid_identifier(&config.channel, 32)
            || !valid_published_at(&config.published_at)
        {
            return Err("release identity, channel, or publish time is invalid".to_string());
        }
        Ok(config)
    }
}

fn usage() -> String {
    "usage: axiusflow_release_publisher --signing-key-file <base64url-key-file> --release-identity <git-head> --generation <n> --base-url <https-release-base> --published-at <UTC-RFC3339> [--channel stable] [--output target/release-publish] [--r2-bucket <bucket>] [--wrangler <command>] [--iscc <Inno Setup compiler>]".to_string()
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
    let repository =
        std::env::current_dir().map_err(|_| "repository directory is unavailable".to_string())?;
    if !repository.join("Cargo.toml").is_file() {
        return Err("run the release publisher from the Axiusflow repository root".to_string());
    }
    verify_repository_identity(&repository, &config.release_identity)?;
    let signing_key = read_signing_key(&config.signing_key_file)?;
    let verifying_key = signing_key.verifying_key();
    build_release_binaries(&repository, &config, &verifying_key)?;
    let binaries = release_binary_paths(&repository);
    let published = package_release(&repository, &config, &signing_key, &binaries)?;
    print_release_summary(&published);
    if let Some(bucket) = config.r2_bucket.as_deref() {
        upload_release(&config, bucket, &published, &verifying_key)?;
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
}

#[derive(Debug)]
struct PublishedRelease {
    release_directory: PathBuf,
    manifest_path: PathBuf,
    channel_path: PathBuf,
    immutable_objects: Vec<UploadObject>,
    channel_object: UploadObject,
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
    let setup_path = release_directory.join(&setup_name);
    let launcher_path = release_directory.join(&launcher_name);
    let desktop_path = release_directory.join(&desktop_name);
    copy_release_binary(&binaries.launcher, &launcher_path)?;
    copy_release_binary(&binaries.desktop, &desktop_path)?;

    let mut files = vec![
        release_file(
            ReleaseFileRole::Desktop,
            &desktop_path,
            &desktop_name,
            &format!("{release_public_root}/{desktop_name}"),
            &config.base_url,
        )?,
        release_file(
            ReleaseFileRole::RuntimeAsset,
            &launcher_path,
            &launcher_name,
            &format!("{release_public_root}/{launcher_name}"),
            &config.base_url,
        )?,
    ];
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = ReleaseManifest {
        schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
        release_identity: config.release_identity.clone(),
        install_generation: config.generation,
        channel: config.channel.clone(),
        minimum_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: platform.to_string(),
        architecture: architecture.to_string(),
        files,
        rollout: RolloutMetadata {
            cohort: "all".to_string(),
            percentage: 100,
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
                &setup_path,
            )?;
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
    let channel = ReleaseChannelPointer {
        schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
        channel: config.channel.clone(),
        platform: platform.to_string(),
        architecture: architecture.to_string(),
        release_identity: config.release_identity.clone(),
        install_generation: config.generation,
        version: env!("CARGO_PKG_VERSION").to_string(),
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
        },
        UploadObject {
            local_path: launcher_path,
            object_key: format!("{release_object_root}/{launcher_name}"),
            content_type: executable_content_type,
        },
        UploadObject {
            local_path: desktop_path,
            object_key: format!("{release_object_root}/{desktop_name}"),
            content_type: executable_content_type,
        },
        UploadObject {
            local_path: manifest_path.clone(),
            object_key: format!("{release_object_root}/manifest.json"),
            content_type: "application/json",
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

fn compile_windows_installer(
    repository: &Path,
    config: &PublisherConfig,
    launcher_path: &Path,
    manifest_path: &Path,
    desktop_path: &Path,
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
        .arg(format!("/DAppVersion={}", env!("CARGO_PKG_VERSION")))
        .arg(format!("/DLauncherPath={}", launcher_path.display()))
        .arg(format!("/DManifestPath={}", manifest_path.display()))
        .arg(format!("/DDesktopPath={}", desktop_path.display()))
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
) -> Result<ReleaseFile, String> {
    let metadata = fs::metadata(path)
        .map_err(|_| "packaged release binary metadata is unavailable".to_string())?;
    Ok(ReleaseFile {
        role,
        path: name.to_string(),
        url: format!("{base_url}/{object_key}"),
        size: metadata.len(),
        sha256: file_sha256_base64url(path)?,
        executable: true,
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
    if path.exists() {
        fs::remove_file(path)
            .map_err(|_| "previous channel metadata could not be replaced".to_string())?;
    }
    fs::rename(staging, path).map_err(|_| "channel metadata could not be committed".to_string())
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

fn upload_release(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    if !valid_identifier(bucket, 128) {
        return Err("R2 bucket name is invalid".to_string());
    }
    let verify_root = config.output_root.join(".r2-verify");
    fs::create_dir_all(&verify_root)
        .map_err(|_| "R2 verification directory could not be created".to_string())?;
    verify_remote_channel_progression(config, bucket, release, verifying_key, &verify_root)?;
    for object in &release.immutable_objects {
        ensure_wrangler_size(object)?;
        match wrangler_get(config, bucket, &object.object_key, &verify_root)? {
            RemoteObject::Missing => {}
            RemoteObject::Downloaded(path) => {
                let _ = fs::remove_file(path);
                return Err(format!(
                    "immutable R2 object already exists: {}",
                    object.object_key
                ));
            }
        }
    }
    for object in &release.immutable_objects {
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
        verify_uploaded_object(object, &downloaded)?;
        fs::remove_file(downloaded)
            .map_err(|_| "R2 verification artifact could not be removed".to_string())?;
        verify_public_object(config, object)?;
    }
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
    verify_uploaded_object(&release.channel_object, &downloaded_channel)?;
    fs::remove_file(downloaded_channel)
        .map_err(|_| "stable channel verification artifact could not be removed".to_string())?;
    let _ = fs::remove_dir(verify_root);
    Ok(())
}

fn verify_remote_channel_progression(
    config: &PublisherConfig,
    bucket: &str,
    release: &PublishedRelease,
    verifying_key: &VerifyingKey,
    verify_root: &Path,
) -> Result<(), String> {
    let remote = wrangler_get(
        config,
        bucket,
        &release.channel_object.object_key,
        verify_root,
    )?;
    let RemoteObject::Downloaded(path) = remote else {
        return Ok(());
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
        verify_release_manifest_signature(&channel.signed_release, verifying_key)
            .map_err(|_| "existing stable channel signature cannot be verified".to_string())?;
        if channel.channel != config.channel
            || channel.platform != std::env::consts::OS
            || channel.architecture != std::env::consts::ARCH
            || channel.install_generation != channel.signed_release.manifest.install_generation
            || channel.release_identity != channel.signed_release.manifest.release_identity
        {
            return Err("existing stable channel identity is inconsistent".to_string());
        }
        if channel.install_generation >= config.generation {
            return Err(format!(
                "stable channel generation {} is not older than candidate generation {}",
                channel.install_generation, config.generation
            ));
        }
        Ok(())
    })();
    let _ = fs::remove_file(path);
    result
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
    let destination = verify_root.join(format!("{suffix}.object"));
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

fn verify_uploaded_object(object: &UploadObject, downloaded: &Path) -> Result<(), String> {
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
    Ok(())
}

fn verify_public_object(config: &PublisherConfig, object: &UploadObject) -> Result<(), String> {
    let public_key = object
        .object_key
        .strip_prefix("releases/")
        .ok_or_else(|| "immutable R2 object is outside the public release namespace".to_string())?;
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

    #[test]
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
            release_identity: "0123456789abcdef0123456789abcdef01234567".to_string(),
            generation: 41,
            base_url: "https://auth.axiusflow.test/releases".to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            channel: "stable".to_string(),
            output_root: root.join("out"),
            r2_bucket: None,
            wrangler: OsString::from("wrangler"),
            iscc: OsString::from("ISCC.exe"),
        };
        let key = SigningKey::from_bytes(&[7; 32]);
        let published = package_release(&root, &config, &key, &binaries).expect("package release");
        let signed: SignedReleaseManifest = serde_json::from_slice(
            &fs::read(&published.manifest_path).expect("signed manifest bytes"),
        )
        .expect("signed manifest");
        verify_release_manifest(&signed, &key.verifying_key(), &ReleasePolicy::native(0))
            .expect("signed release verifies");
        assert_eq!(signed.manifest.files.len(), 2);
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
        assert_eq!(channel.version, env!("CARGO_PKG_VERSION"));
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
            "[UninstallRun]",
            "procedure RegisterExtraCloseApplicationsResources;",
            "RegisterExtraCloseApplicationsResource",
            "--remove-all-local-data",
            "--install \"' + Manifest + '\" \"' + Bundle + '\"",
        ] {
            assert!(
                script.contains(required),
                "installer script lost {required}"
            );
        }
        assert!(!script.contains("Parameters: \"--launch-desktop\""));
    }
}
