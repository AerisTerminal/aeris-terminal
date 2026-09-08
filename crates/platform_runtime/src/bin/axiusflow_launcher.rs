#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! Stable packaging launcher/updater. This binary lives outside version directories.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use axiusflow_platform_runtime::{
    ActiveRelease, CredentialVault, InstallationInventory, LifecycleHooks, NativeCredentialVault,
    RELEASE_CHANNEL_SCHEMA_VERSION, ReleaseChannelPointer, ReleaseFile, ReleaseInstaller,
    ReleasePolicy, SignedReleaseManifest, VaultEntry, native_install_root,
    native_installation_inventory, verify_release_file, verify_release_manifest,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::VerifyingKey;
use sysinfo::{ProcessesToUpdate, System};

// Candidate readiness is a bounded in-process runtime probe. Keep activation
// finite while allowing one complete cold-start attempt on slower machines.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const UPDATE_RESTART_READY: &[u8] = b"AXIUSFLOW_UPDATE_RESTART_READY_V1\n";
const MAXIMUM_INPUT_BYTES: u64 = 1024 * 1024;
const RELEASE_HTTP_TIMEOUT: Duration = Duration::from_mins(10);
const RELEASE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const RELEASE_CHANNEL: &str = "stable";

fn main() {
    let no_arguments = std::env::args_os().len() == 1;
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("Axiusflow lifecycle: {error}");
        if no_arguments {
            show_user_launch_error(&error);
        }
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn show_user_launch_error(error: &str) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = "$shell=New-Object -ComObject WScript.Shell; [void]$shell.Popup($env:AXIUSFLOW_LAUNCH_ERROR,0,'Axiusflow',16)";
    let message = user_launch_error_message(error);
    let _ = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("AXIUSFLOW_LAUNCH_ERROR", message)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(target_os = "windows"))]
fn show_user_launch_error(_error: &str) {}

fn user_launch_error_message(error: &str) -> String {
    let detail: String = error.chars().take(320).collect();
    format!("Axiusflow could not start.\n\n{detail}")
}

fn run(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(redacted)?;
    let command = arguments
        .next()
        .and_then(|argument| argument.into_string().ok());
    let verifying_key = embedded_verifying_key()?;
    if command.is_none() {
        require_no_more(arguments)?;
        return bootstrap_update_and_launch(&executable, &verifying_key);
    }
    if command.as_deref() == Some("--promote-stable-launcher") {
        require_no_more(arguments)?;
        return promote_stable_launcher(&executable, &verifying_key);
    }
    let install_root = executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "stable launcher installation root is unavailable".to_string())?;
    let installer = ReleaseInstaller::new(&install_root, verifying_key, ReleasePolicy::native(0))
        .map_err(|error| error.to_string())?;
    let hooks = NativeHooks {
        install_root: install_root.clone(),
    };
    match command.as_deref() {
        Some("--launch-desktop") => {
            require_no_more(arguments)?;
            installer.recover(&hooks).map_err(|error| error.to_string())?;
            launch_active(&installer, "axiusflow_desktop")
        }
        Some("--install") => {
            let manifest_path = required_path(&mut arguments, "signed manifest")?;
            let bundle_root = required_path(&mut arguments, "release bundle")?;
            require_no_more(arguments)?;
            let signed: SignedReleaseManifest = read_bounded_json(&manifest_path)?;
            installer.recover(&hooks).map_err(|error| error.to_string())?;
            if installer
                .audit_active_release()
                .map_err(|error| error.to_string())?
                .is_some_and(|active| {
                    active.install_generation == signed.manifest.install_generation
                        && active.release_identity == signed.manifest.release_identity
                })
            {
                // A user may re-run the same standard Windows installer. The
                // active release audit above already verified the installed
                // inventory; still authenticate the bundled manifest before
                // treating this as an idempotent successful install.
                verify_release_manifest(
                    &signed,
                    &verifying_key,
                    &ReleasePolicy::native(signed.manifest.install_generation),
                )
                .map_err(|error| error.to_string())?;
                return Ok(());
            }
            installer
                .install(&signed, &bundle_root, &hooks)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        Some("--update") => {
            require_no_more(arguments)?;
            installer.recover(&hooks).map_err(|error| error.to_string())?;
            install_remote_update(&installer, &verifying_key, &hooks, &install_root)
        }
        Some("--update-and-restart") => {
            require_no_more(arguments)?;
            update_and_restart(&installer, &verifying_key, &hooks, &install_root)
        }
        Some("--check-update") => {
            require_no_more(arguments)?;
            check_remote_update(&installer, &verifying_key)
        }
        Some("--recover") => {
            require_no_more(arguments)?;
            installer.recover(&hooks).map_err(|error| error.to_string())
        }
        Some("--remove-all-local-data") => {
            require_no_more(arguments)?;
            // The launcher runs from inside the tree this command deletes,
            // and Windows refuses to delete a running executable (proven by
            // the failed campaign: the child could never remove its waiting
            // parent). Rename this image to a sibling staging directory
            // first — renames are permitted — then uninstall the original
            // tree synchronously with an honest exit code. A detached OS
            // command removes the staging copy afterwards on Windows; Unix
            // unlinks it directly before exit.
            let staged = relocate_running_binary(&executable, &install_root)
                .map_err(redacted)?;
            let result = uninstall_from_root(&install_root);
            remove_relocated_binary(&staged);
            result
        }
        _ => Err("usage: axiusflow_launcher <--launch-desktop|--install <manifest> <bundle>|--update|--update-and-restart|--check-update|--recover|--remove-all-local-data|--promote-stable-launcher>".to_string()),
    }
}

fn bootstrap_update_and_launch(
    executable: &Path,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let install_root = native_install_root().map_err(|error| error.to_string())?;
    fs::create_dir_all(&install_root)
        .map_err(|_| "per-user Axiusflow installation root could not be created".to_string())?;
    // Validate the ownership root before copying the bootstrap into it. This
    // rejects a pre-created symlinked install root before the first write.
    let installer = ReleaseInstaller::new(&install_root, *verifying_key, ReleasePolicy::native(0))
        .map_err(|error| error.to_string())?;
    let stable_launcher = stable_launcher_path(&install_root);
    if executable != stable_launcher {
        persist_stable_launcher(executable, &stable_launcher)?;
    }
    let hooks = NativeHooks {
        install_root: install_root.clone(),
    };
    installer
        .recover(&hooks)
        .map_err(|error| error.to_string())?;
    let had_active_release = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .is_some();
    if let Err(update_error) =
        install_remote_update(&installer, verifying_key, &hooks, &install_root)
    {
        if !had_active_release {
            return Err(update_error);
        }
        // Axiusflow is local-first: an already verified installation remains
        // launchable while offline or when the release service is temporarily
        // unavailable. If an update transaction had started, recover it before
        // selecting the active release again.
        installer
            .recover(&hooks)
            .map_err(|error| error.to_string())?;
        eprintln!("Axiusflow update deferred: {update_error}");
    }
    launch_active(&installer, "axiusflow_desktop")
}

#[cfg(target_os = "windows")]
fn windows_start_menu_shortcut() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("Microsoft/Windows/Start Menu/Programs/Axiusflow/Axiusflow.lnk"))
        .ok_or_else(|| "Windows Start Menu location is unavailable".to_string())
}

#[cfg(target_os = "windows")]
fn remove_launcher_registration() -> Result<(), String> {
    let shortcut = windows_start_menu_shortcut()?;
    match fs::remove_file(&shortcut) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Windows Start Menu shortcut could not be removed".to_string()),
    }
    if let Some(parent) = shortcut.parent() {
        let _ = fs::remove_dir(parent);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn remove_launcher_registration() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn launcher_registration_absent() -> Result<bool, String> {
    Ok(!windows_start_menu_shortcut()?.exists())
}

#[cfg(not(target_os = "windows"))]
fn launcher_registration_absent() -> Result<bool, String> {
    Ok(true)
}

fn persist_stable_launcher(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|_| "setup executable metadata is unavailable".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("setup executable is not a regular file".to_string());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "stable launcher destination is invalid".to_string())?;
    let staging = parent.join(format!(
        ".axiusflow_launcher{}.next",
        std::env::consts::EXE_SUFFIX
    ));
    let _ = fs::remove_file(&staging);
    fs::copy(source, &staging).map_err(|_| "stable launcher staging copy failed".to_string())?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&staging)
        .and_then(|file| file.sync_all())
        .map_err(|_| "stable launcher staging copy could not be committed".to_string())?;
    fs::rename(&staging, destination)
        .map_err(|_| "stable launcher could not be committed".to_string())
}

fn stable_launcher_path(install_root: &Path) -> PathBuf {
    install_root.join(format!(
        "axiusflow_launcher{}",
        std::env::consts::EXE_SUFFIX
    ))
}

fn promote_stable_launcher(executable: &Path, verifying_key: &VerifyingKey) -> Result<(), String> {
    let install_root = native_install_root().map_err(|error| error.to_string())?;
    let stable_launcher = stable_launcher_path(&install_root);
    if executable == stable_launcher {
        return Ok(());
    }

    let installer = ReleaseInstaller::new(&install_root, *verifying_key, ReleasePolicy::native(0))
        .map_err(|error| error.to_string())?;
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no verified Axiusflow release is active".to_string())?;
    let expected = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!(
            "axiusflow_launcher{}",
            std::env::consts::EXE_SUFFIX
        ));
    let executable = fs::canonicalize(executable)
        .map_err(|_| "versioned launcher path could not be verified".to_string())?;
    let expected = fs::canonicalize(expected)
        .map_err(|_| "signed versioned launcher is unavailable".to_string())?;
    if executable != expected {
        return Err("launcher promotion source is not the active signed release".to_string());
    }

    if files_match(&executable, &stable_launcher)? {
        return Ok(());
    }

    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while process_is_running(&stable_launcher) {
        if Instant::now() >= deadline {
            return Err("stable launcher did not exit before promotion".to_string());
        }
        thread::sleep(Duration::from_millis(20));
    }
    persist_stable_launcher(&executable, &stable_launcher)
}

fn files_match(left: &Path, right: &Path) -> Result<bool, String> {
    let left_metadata = fs::symlink_metadata(left)
        .map_err(|_| "versioned launcher metadata is unavailable".to_string())?;
    if !left_metadata.is_file() || left_metadata.file_type().is_symlink() {
        return Err("versioned launcher is not a regular file".to_string());
    }
    let right_metadata = match fs::symlink_metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("stable launcher metadata is unavailable".to_string()),
    };
    if !right_metadata.is_file() || right_metadata.file_type().is_symlink() {
        return Ok(false);
    }
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }

    let mut left = File::open(left).map_err(redacted)?;
    let mut right = File::open(right).map_err(redacted)?;
    let mut left_buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut right_buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let left_count = left.read(&mut left_buffer).map_err(redacted)?;
        let right_count = right.read(&mut right_buffer).map_err(redacted)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

fn install_remote_update(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    hooks: &NativeHooks,
    install_root: &Path,
) -> Result<(), String> {
    let (active, channel) = checked_release_channel(installer, verifying_key)?;
    let signed = &channel.signed_release;
    if reconcile_same_generation_launcher(
        active.as_ref(),
        signed.manifest.install_generation,
        || spawn_active_launcher_promotion(installer),
    ) {
        return Ok(());
    }

    let downloads_root = install_root.join(".release-downloads");
    prepare_secure_directory(&downloads_root)?;
    let bundle_root = downloads_root.join(format!(
        "{:020}-{}",
        signed.manifest.install_generation, signed.manifest.release_identity
    ));
    prepare_secure_directory(&bundle_root)?;
    let download_agent = release_http_agent(RELEASE_HTTP_TIMEOUT);
    for file in &signed.manifest.files {
        download_release_file(&download_agent, &bundle_root, file)?;
    }
    installer
        .install(signed, &bundle_root, hooks)
        .map_err(|error| error.to_string())?;
    if let Err(error) = spawn_active_launcher_promotion(installer) {
        eprintln!("Axiusflow launcher promotion deferred: {error}");
    }
    fs::remove_dir_all(&bundle_root)
        .map_err(|_| "release installed but its download cache could not be removed".to_string())?;
    if fs::read_dir(&downloads_root).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(downloads_root);
    }
    Ok(())
}

fn reconcile_same_generation_launcher<F>(
    active: Option<&ActiveRelease>,
    target_generation: u64,
    promote: F,
) -> bool
where
    F: FnOnce() -> Result<(), String>,
{
    if active.is_none_or(|active| active.install_generation != target_generation) {
        return false;
    }
    // Promotion is best-effort after a new install because the prior stable
    // launcher may still be the process performing that install. Retry it on
    // every same-generation launch so one transient failure cannot strand an
    // old launcher that lacks newer lifecycle commands.
    if let Err(error) = promote() {
        eprintln!("Axiusflow launcher promotion deferred: {error}");
    }
    true
}

fn update_and_restart(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    hooks: &NativeHooks,
    install_root: &Path,
) -> Result<(), String> {
    let desktop = preflight_update_restart(installer, verifying_key)?;
    announce_update_restart_ready()?;
    wait_for_desktop_stop(&desktop)?;

    let update_result = (|| {
        // Recovery may run candidate health checks, so it must happen only
        // after the interactive desktop that requested the handoff is gone.
        installer
            .recover(hooks)
            .map_err(|error| error.to_string())?;
        install_remote_update(installer, verifying_key, hooks, install_root)
    })();
    if let Err(update_error) = update_result {
        eprintln!("Axiusflow update deferred: {update_error}");
        // The desktop has already yielded ownership to this launcher. Recover
        // whatever transaction state is safely recoverable, then relaunch the
        // verified active release so a transient update failure does not make
        // the application disappear.
        let recovery_error = installer.recover(hooks).err();
        match installer.audit_active_release() {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(format!(
                    "{update_error}; no verified Axiusflow release is active{}",
                    recovery_error.map_or_else(String::new, |error| {
                        format!("; update recovery failed: {error}")
                    })
                ));
            }
            Err(error) => {
                return Err(format!(
                    "{update_error}; active release audit failed: {error}{}",
                    recovery_error.map_or_else(String::new, |error| {
                        format!("; update recovery failed: {error}")
                    })
                ));
            }
        }
    }
    launch_active(installer, "axiusflow_desktop")
}

fn preflight_update_restart(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
) -> Result<PathBuf, String> {
    let (active, channel) = checked_release_channel(installer, verifying_key)?;
    let active = active.ok_or_else(|| "no verified Axiusflow release is active".to_string())?;
    if channel.install_generation == active.install_generation {
        return Err("no newer Axiusflow update is available".to_string());
    }
    let desktop = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX));
    let metadata = fs::symlink_metadata(&desktop)
        .map_err(|_| "active Axiusflow desktop is unavailable".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("active Axiusflow desktop is invalid".to_string());
    }
    Ok(desktop)
}

fn announce_update_restart_ready() -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(UPDATE_RESTART_READY)
        .and_then(|()| stdout.flush())
        .map_err(|_| "update restart acknowledgement could not be written".to_string())
}

fn wait_for_desktop_stop(desktop: &Path) -> Result<(), String> {
    let deadline = Instant::now() + RESTART_WAIT_TIMEOUT;
    while process_is_running(desktop) {
        if Instant::now() >= deadline {
            return Err("active Axiusflow desktop did not close for restart".to_string());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn checked_release_channel(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
) -> Result<(Option<ActiveRelease>, ReleaseChannelPointer), String> {
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?;
    let base_url = embedded_release_base_url()?;
    let manifest_url = format!(
        "{base_url}/channels/{RELEASE_CHANNEL}/{}/{}.json",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let discovery_agent = release_http_agent(RELEASE_DISCOVERY_TIMEOUT);
    let channel = fetch_release_channel(&discovery_agent, &manifest_url)?;
    let signed = &channel.signed_release;

    // The mutable channel object is untrusted transport data until the
    // offline release key authenticates the complete manifest, including all
    // artifact URLs and hashes. No file URL is issued before this succeeds.
    let active_generation = active
        .as_ref()
        .map_or(0, |release| release.install_generation);
    let minimum_generation = bootstrap_minimum_generation()?.max(active_generation);
    verify_release_manifest(
        signed,
        verifying_key,
        &ReleasePolicy::native(minimum_generation),
    )
    .map_err(|error| error.to_string())?;
    validate_release_channel(&channel, base_url)?;
    if signed.manifest.channel != RELEASE_CHANNEL {
        return Err("release channel manifest does not match the stable channel".to_string());
    }
    if signed.manifest.rollout.cohort != "all" || signed.manifest.rollout.percentage != 100 {
        return Err("stable release manifest uses an unsupported partial rollout".to_string());
    }
    if let Some(active) = &active
        && signed.manifest.install_generation < active.install_generation
    {
        return Err("release channel generation regressed below the active release".to_string());
    }
    Ok((active, channel))
}

#[derive(serde::Serialize)]
struct UpdateCheckReport {
    schema_version: u32,
    current_generation: u64,
    latest_generation: u64,
    current_version: String,
    latest_version: String,
    update_available: bool,
}

fn update_check_report(
    current_generation: u64,
    latest_generation: u64,
    latest_version: &str,
) -> UpdateCheckReport {
    UpdateCheckReport {
        schema_version: 2,
        current_generation,
        latest_generation,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        latest_version: latest_version.to_string(),
        update_available: latest_generation > current_generation,
    }
}

fn check_remote_update(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let (active, channel) = checked_release_channel(installer, verifying_key)?;
    let current_generation = active
        .as_ref()
        .map_or(0, |release| release.install_generation);
    let report = update_check_report(
        current_generation,
        channel.install_generation,
        &channel.version,
    );
    let encoded = serde_json::to_string(&report)
        .map_err(|_| "update status could not be encoded".to_string())?;
    println!("{encoded}");
    Ok(())
}

fn spawn_active_launcher_promotion(installer: &ReleaseInstaller) -> Result<(), String> {
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no verified Axiusflow release is active".to_string())?;
    let launcher = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!(
            "axiusflow_launcher{}",
            std::env::consts::EXE_SUFFIX
        ));
    let metadata = fs::symlink_metadata(&launcher)
        .map_err(|_| "signed versioned launcher is unavailable".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("signed versioned launcher is invalid".to_string());
    }
    Command::new(launcher)
        .arg("--promote-stable-launcher")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "versioned launcher promotion could not be scheduled".to_string())
}

fn bootstrap_minimum_generation() -> Result<u64, String> {
    let encoded = option_env!("AXIUSFLOW_BOOTSTRAP_MIN_GENERATION").ok_or_else(|| {
        "bootstrap minimum release generation was not embedded by packaging".to_string()
    })?;
    encoded
        .parse::<u64>()
        .ok()
        .filter(|generation| *generation > 0)
        .ok_or_else(|| "embedded bootstrap minimum release generation is invalid".to_string())
}

fn release_http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .max_idle_connections(2)
        .max_idle_connections_per_host(2)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn embedded_release_base_url() -> Result<&'static str, String> {
    let url = option_env!("AXIUSFLOW_RELEASE_BASE_URL")
        .ok_or_else(|| "release base URL was not embedded by packaging".to_string())?;
    if !valid_https_url(url) || url.ends_with('/') || url.contains('?') {
        return Err("embedded release base URL is invalid".to_string());
    }
    Ok(url)
}

fn valid_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    !rest.is_empty()
        && !rest.starts_with('/')
        && !url.contains([' ', '\n', '\r', '\t'])
        && !url.contains('#')
}

fn fetch_release_channel(agent: &ureq::Agent, url: &str) -> Result<ReleaseChannelPointer, String> {
    if !valid_https_url(url) {
        return Err("release channel URL is invalid".to_string());
    }
    let mut response = agent
        .get(url)
        .call()
        .map_err(|_| "release channel request failed".to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAXIMUM_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "release channel response is unreadable".to_string())?;
    if bytes.len() as u64 > MAXIMUM_INPUT_BYTES {
        return Err("release channel response exceeds the size bound".to_string());
    }
    serde_json::from_slice(&bytes).map_err(|_| "release channel response is malformed".to_string())
}

fn validate_release_channel(channel: &ReleaseChannelPointer, base_url: &str) -> Result<(), String> {
    let manifest = &channel.signed_release.manifest;
    if channel.schema_version != RELEASE_CHANNEL_SCHEMA_VERSION
        || channel.channel != manifest.channel
        || channel.platform != manifest.platform
        || channel.architecture != manifest.architecture
        || channel.release_identity != manifest.release_identity
        || channel.install_generation != manifest.install_generation
        || !valid_release_identity(&channel.release_identity)
        || !valid_release_version(&channel.version)
        || !valid_published_at(&channel.published_at)
    {
        return Err("release channel metadata does not match its signed release".to_string());
    }
    let release_key = format!(
        "{}-{}",
        manifest.install_generation, manifest.release_identity
    );
    let release_root = format!(
        "{base_url}/{}/{}/{release_key}",
        manifest.platform, manifest.architecture
    );
    let expected_manifest_url = format!("{release_root}/manifest.json");
    if channel.manifest_url != expected_manifest_url || !valid_https_url(&channel.manifest_url) {
        return Err("release channel manifest URL is invalid".to_string());
    }
    let expected_installer_name = format!("Axiusflow-Setup{}", std::env::consts::EXE_SUFFIX);
    let installer = &channel.installer;
    let expected_installer_url = format!("{release_root}/{expected_installer_name}");
    let installer_digest = URL_SAFE_NO_PAD
        .decode(&installer.sha256_b64url)
        .map_err(|_| "release channel installer hash is invalid".to_string())?;
    if installer.filename != expected_installer_name
        || installer.url != expected_installer_url
        || installer.size == 0
        || installer_digest.len() != 32
    {
        return Err("release channel installer metadata is invalid".to_string());
    }
    for file in &manifest.files {
        let expected_url = format!("{release_root}/{}", file.path);
        if file.url != expected_url {
            return Err(
                "signed release artifact URL is outside its immutable release path".to_string(),
            );
        }
    }
    Ok(())
}

fn valid_release_identity(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_release_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'+' | b'-'))
        })
}

fn valid_published_at(value: &str) -> bool {
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

fn download_release_file(
    agent: &ureq::Agent,
    bundle_root: &Path,
    expected: &ReleaseFile,
) -> Result<(), String> {
    if !valid_https_url(&expected.url) {
        return Err("signed release artifact URL is invalid".to_string());
    }
    let relative = Path::new(&expected.path);
    prepare_release_parent(bundle_root, relative)?;
    let final_path = bundle_root.join(relative);
    let parent = final_path
        .parent()
        .ok_or_else(|| "release artifact path is invalid".to_string())?;
    debug_assert!(parent.starts_with(bundle_root));
    if final_path.exists() {
        if verify_release_file(&final_path, expected).is_ok() {
            return Ok(());
        }
        fs::remove_file(&final_path)
            .map_err(|_| "invalid cached release artifact could not be removed".to_string())?;
    }

    let partial_path = partial_download_path(&final_path)?;
    let mut offset = partial_length(&partial_path, expected.size)?;
    if offset == expected.size {
        if verify_release_file(&partial_path, expected).is_ok() {
            fs::rename(&partial_path, &final_path)
                .map_err(|_| "verified release artifact could not be committed".to_string())?;
            return Ok(());
        }
        truncate_file(&partial_path)?;
        offset = 0;
    }

    let mut request = agent.get(&expected.url);
    if offset > 0 {
        request = request.header("Range", format!("bytes={offset}-"));
    }
    let mut response = request
        .call()
        .map_err(|_| "release artifact request failed".to_string())?;
    let status = response.status().as_u16();
    let append = if offset == 0 {
        if status != 200 {
            return Err("release artifact server returned an invalid status".to_string());
        }
        false
    } else if status == 206 {
        validate_content_range(response.headers(), offset, expected.size)?;
        true
    } else if status == 200 {
        // The origin ignored Range. Restart safely from the full response
        // rather than combining incompatible byte streams.
        truncate_file(&partial_path)?;
        offset = 0;
        false
    } else {
        return Err("release artifact resume response is invalid".to_string());
    };

    write_bounded_download(
        response.body_mut().as_reader(),
        &partial_path,
        offset,
        expected.size,
        append,
    )?;
    if verify_release_file(&partial_path, expected).is_err() {
        let _ = fs::remove_file(&partial_path);
        return Err("release artifact hash verification failed".to_string());
    }
    fs::rename(&partial_path, &final_path)
        .map_err(|_| "verified release artifact could not be committed".to_string())?;
    Ok(())
}

fn prepare_release_parent(bundle_root: &Path, relative: &Path) -> Result<(), String> {
    prepare_secure_directory(bundle_root)?;
    let mut current = bundle_root.to_path_buf();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let Component::Normal(component) = component else {
                return Err("release artifact path is invalid".to_string());
            };
            current.push(component);
            prepare_secure_directory(&current)?;
        }
    }
    Ok(())
}

fn prepare_secure_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("release download directory is invalid".to_string());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|_| "release download directory could not be created".to_string())?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|_| "release download directory is invalid".to_string())?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("release download directory is invalid".to_string());
            }
        }
        Err(_) => return Err("release download directory is invalid".to_string()),
    }
    Ok(())
}

fn partial_download_path(final_path: &Path) -> Result<PathBuf, String> {
    let name = final_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "release artifact filename is invalid".to_string())?;
    Ok(final_path.with_file_name(format!("{name}.part")))
}

fn partial_length(path: &Path, maximum: u64) -> Result<u64, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(_) => return Err("release partial metadata is unavailable".to_string()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("release partial artifact is invalid".to_string());
    }
    if metadata.len() > maximum {
        truncate_file(path)?;
        return Ok(0);
    }
    Ok(metadata.len())
}

fn truncate_file(path: &Path) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| "release partial artifact could not be reset".to_string())
}

fn validate_content_range(
    headers: &ureq::http::HeaderMap,
    offset: u64,
    expected_size: u64,
) -> Result<(), String> {
    let value = headers
        .get("Content-Range")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "release resume response omitted Content-Range".to_string())?;
    let expected_prefix = format!("bytes {offset}-");
    let expected_value = format!("{expected_prefix}{}/{expected_size}", expected_size - 1);
    if value != expected_value {
        return Err("release resume Content-Range does not match the artifact".to_string());
    }
    Ok(())
}

fn write_bounded_download(
    reader: impl Read,
    path: &Path,
    offset: u64,
    expected_size: u64,
    append: bool,
) -> Result<(), String> {
    let remaining = expected_size
        .checked_sub(offset)
        .ok_or_else(|| "release artifact offset exceeds its signed size".to_string())?;
    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .map_err(|_| "release partial artifact could not be opened".to_string())?;
    let mut limited = reader.take(remaining.saturating_add(1));
    let Ok(copied) = std::io::copy(&mut limited, &mut output) else {
        let _ = output.flush().and_then(|()| output.sync_all());
        return Err("release artifact download was interrupted".to_string());
    };
    output
        .flush()
        .and_then(|()| output.sync_all())
        .map_err(|_| "release partial artifact could not be committed".to_string())?;
    if copied > remaining {
        return Err("release artifact response exceeded its signed size".to_string());
    }
    if offset.saturating_add(copied) != expected_size {
        return Err("release artifact download is incomplete".to_string());
    }
    Ok(())
}

/// Renames the running launcher to a sibling staging directory outside
/// `install_root`. Same-volume renames are permitted on Windows even for a
/// running image, while deleting or overwriting it is denied — so the
/// uninstall can then remove the original tree synchronously with an
/// honest exit code. Fails closed before touching anything.
fn relocate_running_binary(
    executable: &Path,
    install_root: &Path,
) -> Result<PathBuf, std::io::Error> {
    let name = install_root
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let dir = install_root
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
        .join(format!(".{}-uninstall-stage", name.to_string_lossy()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let staged = dir.join(format!(
        "axiusflow_uninstall{}",
        std::env::consts::EXE_SUFFIX
    ));
    fs::rename(executable, &staged)?;
    Ok(staged)
}

/// Removes the relocated staging copy after the owned tree is gone. Unix
/// unlinks the running image directly; Windows schedules an OS-owned
/// delayed delete because the image stays mapped until process exit.
fn remove_relocated_binary(staged: &Path) {
    #[cfg(unix)]
    {
        let _ = fs::remove_file(staged);
        if let Some(dir) = staged.parent() {
            let _ = fs::remove_dir(dir);
        }
    }
    #[cfg(not(unix))]
    {
        // The command must live in a batch file, not argv: Rust quotes an
        // argv element containing spaces, and cmd then strips the outer
        // pair while leaving Rust-style \" escapes behind, silently
        // breaking every quoted path (proven by two failed campaigns whose
        // inline commands never deleted anything). A batch file has no
        // quoting layer: `cmd /c "<bat>"` is the one standard case that
        // parses. The log lives inside staging so success leaves no trace
        // while failure keeps its own evidence.
        let Some(dir) = staged.parent() else { return };
        let log = dir.join("deleter.log");
        let bat = dir.join("finish-uninstall.bat");
        let body = format!(
            "@echo off\r\necho deleter-start>>\"{2}\"\r\nping -n 4 127.0.0.1 >nul\r\nfor /l %%i in (1,1,20) do (\r\ndel /q \"{0}\" 2>nul\r\nif not exist \"{0}\" (\r\nrmdir /s /q \"{1}\" 2>nul\r\nexit 0\r\n)\r\nping -n 2 127.0.0.1 >nul\r\n)\r\necho deleter-gave-up>>\"{2}\"\r\n",
            staged.display(),
            dir.display(),
            log.display()
        );
        if fs::write(&bat, body).is_err() {
            return;
        }
        let _ = Command::new("cmd")
            .arg("/c")
            .arg(&bat)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Uninstalls the explicit root. Used by the relocated child; factored so
/// tests can drive it with a test key without embedding one.
fn uninstall_from_root_with_key(root: &Path, verifying_key: VerifyingKey) -> Result<(), String> {
    let installer = ReleaseInstaller::new(root, verifying_key, ReleasePolicy::native(0))
        .map_err(|error| error.to_string())?;
    let hooks = NativeHooks {
        install_root: root.to_path_buf(),
    };
    let inventory = native_installation_inventory(root).map_err(|error| error.to_string())?;
    installer
        .uninstall(&inventory, &hooks)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn uninstall_from_root(root: &Path) -> Result<(), String> {
    uninstall_from_root_with_key(root, embedded_verifying_key()?)
}

fn embedded_verifying_key() -> Result<VerifyingKey, String> {
    let encoded = option_env!("AXIUSFLOW_RELEASE_VERIFYING_KEY")
        .ok_or_else(|| "release verification key was not embedded by packaging".to_string())?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "embedded release verification key is invalid".to_string())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "embedded release verification key is invalid".to_string())?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| "embedded release verification key is invalid".to_string())
}

fn launch_active(installer: &ReleaseInstaller, binary: &str) -> Result<(), String> {
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no verified Axiusflow release is active".to_string())?;
    let executable = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!("{binary}{}", std::env::consts::EXE_SUFFIX));
    Command::new(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "active Axiusflow process could not be started".to_string())
}

fn required_path(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    description: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{description} path is required"))
}

fn require_no_more(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    if arguments.next().is_none() {
        Ok(())
    } else {
        Err("unexpected lifecycle argument".to_string())
    }
}

fn read_bounded_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let metadata = fs::symlink_metadata(path).map_err(redacted)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAXIMUM_INPUT_BYTES
    {
        return Err("signed lifecycle input is invalid".to_string());
    }
    let bytes = fs::read(path).map_err(redacted)?;
    serde_json::from_slice(&bytes).map_err(|_| "signed lifecycle input is invalid".to_string())
}

struct NativeHooks {
    install_root: PathBuf,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopReadinessReport {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
    desktop_process_id: u32,
    workspace_revision: u64,
    provider_count: usize,
    workspace_restored: bool,
    market_service_ready: bool,
    account_runtime_ready: bool,
}

impl NativeHooks {
    fn desktop(&self, release: &ActiveRelease) -> PathBuf {
        self.install_root
            .join("versions")
            .join(&release.directory_name)
            .join(format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX))
    }

    fn wait_for_child(mut child: std::process::Child) -> Result<(), String> {
        let deadline = Instant::now() + HEALTH_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().map_err(redacted)? {
                return if status.success() {
                    Ok(())
                } else {
                    Err("candidate desktop readiness probe failed".to_string())
                };
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("candidate desktop readiness probe timed out".to_string());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl LifecycleHooks for NativeHooks {
    fn prepare_activation(&self, previous: Option<&ActiveRelease>) -> Result<(), String> {
        if let Some(previous) = previous {
            let desktop = self.desktop(previous);
            let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
            while process_is_running(&desktop) {
                if Instant::now() >= deadline {
                    return Err("active Axiusflow desktop must close before update".to_string());
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        Ok(())
    }

    fn health_check(&self, candidate: &ActiveRelease) -> Result<(), String> {
        let report = self
            .install_root
            .join("versions")
            .join(&candidate.directory_name)
            .join("readiness-report.json");
        let _ = fs::remove_file(&report);
        let result = (|| {
            let child = Command::new(self.desktop(candidate))
                .arg("--desktop-readiness")
                .arg(&report)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(redacted)?;
            Self::wait_for_child(child)?;
            let readiness: DesktopReadinessReport = read_bounded_json(&report)?;
            if readiness.schema_version != 2
                || readiness.release_identity != candidate.release_identity
                || readiness.install_generation != candidate.install_generation
                || readiness.desktop_process_id == 0
                || readiness.provider_count == 0
                || !readiness.workspace_restored
                || !readiness.market_service_ready
                || !readiness.account_runtime_ready
            {
                return Err("candidate desktop readiness report is invalid".to_string());
            }
            let _ = readiness.workspace_revision;
            Ok(())
        })();
        let _ = fs::remove_file(report);
        result
    }

    fn disable_registrations(&self, registrations: &[String]) -> Result<(), String> {
        if registrations
            .iter()
            .any(|entry| entry == "start-menu:Axiusflow")
        {
            remove_launcher_registration()?;
        }
        Ok(())
    }

    fn stop_owned_processes(&self) -> Result<(), String> {
        if let Some(active) = active_from_root(&self.install_root)? {
            self.prepare_activation(Some(&active))?;
        }
        Ok(())
    }

    fn delete_vault_entry(&self, entry: &VaultEntry) -> Result<(), String> {
        NativeCredentialVault::new(&entry.service)
            .and_then(|vault| vault.delete(&entry.key))
            .map_err(redacted)
    }

    fn audit_external_absence(&self, inventory: &InstallationInventory) -> Result<(), String> {
        if owned_process_is_running(&self.install_root) {
            return Err("an Axiusflow process remains active".to_string());
        }
        if inventory
            .registrations
            .iter()
            .any(|entry| entry == "start-menu:Axiusflow")
            && !launcher_registration_absent()?
        {
            return Err("an Axiusflow Start Menu shortcut remains".to_string());
        }
        for entry in &inventory.vault_entries {
            let vault = NativeCredentialVault::new(&entry.service).map_err(redacted)?;
            if vault.load(&entry.key).map_err(redacted)?.is_some() {
                return Err("an Axiusflow vault entry remains".to_string());
            }
        }
        Ok(())
    }
}

fn active_from_root(root: &Path) -> Result<Option<ActiveRelease>, String> {
    let installer =
        ReleaseInstaller::new(root, embedded_verifying_key()?, ReleasePolicy::native(0))
            .map_err(|error| error.to_string())?;
    installer
        .audit_active_release()
        .map_err(|error| error.to_string())
}

fn process_is_running(executable: &Path) -> bool {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .any(|process| process.exe() == Some(executable))
}

fn owned_process_is_running(install_root: &Path) -> bool {
    let versions = install_root.join("versions");
    let desktop = format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.processes().values().any(|process| {
        process.exe().is_some_and(|executable| {
            executable.starts_with(&versions)
                && executable.file_name() == Some(std::ffi::OsStr::new(&desktop))
        })
    })
}

fn redacted<E>(_error: E) -> String {
    "native Axiusflow lifecycle operation failed".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_platform_runtime::{
        RELEASE_MANIFEST_SCHEMA_VERSION, ReleaseFileRole, ReleaseInstallerMetadata,
        ReleaseManifest, RolloutMetadata, sign_release_manifest,
    };
    use ed25519_dalek::SigningKey;

    fn temporary_base(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let base = std::env::temp_dir().join(format!("launcher-{name}-{nanos}"));
        fs::create_dir_all(&base).expect("temporary base");
        base
    }

    #[test]
    fn relocate_moves_binary_outside_install_root() {
        // Fixture copies only: the test binary itself is never renamed.
        let base = temporary_base("relocate");
        let root = base.join("root");
        fs::create_dir_all(&root).expect("install root");
        let fixture = root.join(format!("launcher{}", std::env::consts::EXE_SUFFIX));
        let executable = std::env::current_exe().expect("current executable");
        fs::copy(&executable, &fixture).expect("fixture binary");
        let staged = relocate_running_binary(&fixture, &root).expect("relocate");
        assert!(!fixture.exists());
        assert!(staged.exists());
        assert!(!staged.starts_with(&root));
        assert_eq!(
            fs::read(&staged).expect("staged bytes"),
            fs::read(&executable).expect("original bytes")
        );
        fs::remove_file(&staged).expect("remove staging copy");
        fs::remove_dir_all(&base).expect("remove temporary base");
        // Note: `remove_relocated_binary` is proven by the physical
        // installed-lifecycle campaign: its Windows path schedules an
        // OS-owned delayed delete that a unit test cannot await
        // deterministically.
    }

    #[test]
    fn stable_launcher_persistence_replaces_existing_copy() {
        let base = temporary_base("stable-replace");
        let source = base.join("source.exe");
        let destination = base.join("axiusflow_launcher.exe");
        fs::write(&source, b"new-launcher").expect("source fixture");
        fs::write(&destination, b"old-launcher").expect("destination fixture");

        persist_stable_launcher(&source, &destination).expect("replace stable launcher");

        assert_eq!(
            fs::read(&destination).expect("committed launcher"),
            b"new-launcher"
        );
        assert!(!base.join(".axiusflow_launcher.exe.next").exists());
        fs::remove_dir_all(base).expect("remove temporary base");
    }

    #[test]
    fn launcher_content_comparison_is_exact() {
        let base = temporary_base("content-match");
        let left = base.join("left.exe");
        let right = base.join("right.exe");
        fs::write(&left, b"same").expect("left fixture");
        fs::write(&right, b"same").expect("right fixture");
        assert!(files_match(&left, &right).expect("matching files"));
        fs::write(&right, b"diff").expect("replace right fixture");
        assert!(!files_match(&left, &right).expect("different files"));
        fs::remove_dir_all(base).expect("remove temporary base");
    }

    #[test]
    fn same_generation_update_retries_launcher_promotion_once() {
        let active = ActiveRelease {
            release_identity: "current".to_string(),
            install_generation: 7,
            directory_name: "00000000000000000007-current".to_string(),
        };
        let promotions = std::cell::Cell::new(0_u32);
        assert!(reconcile_same_generation_launcher(Some(&active), 7, || {
            promotions.set(promotions.get() + 1);
            Ok(())
        }));
        assert_eq!(promotions.get(), 1);

        assert!(!reconcile_same_generation_launcher(
            Some(&active),
            8,
            || {
                promotions.set(promotions.get() + 1);
                Ok(())
            }
        ));
        assert_eq!(promotions.get(), 1);
    }

    #[test]
    fn stable_channel_requires_exact_website_envelope_and_immutable_paths() {
        let base_url = "https://auth.axiusflow.test/releases";
        let identity = "0123456789abcdef0123456789abcdef01234567";
        let release_root = format!(
            "{base_url}/{}/{}/7-{identity}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let digest = URL_SAFE_NO_PAD.encode([3_u8; 32]);
        let suffix = std::env::consts::EXE_SUFFIX;
        let files = vec![ReleaseFile {
            role: ReleaseFileRole::Desktop,
            path: format!("axiusflow_desktop{suffix}"),
            url: format!("{release_root}/axiusflow_desktop{suffix}"),
            size: 10,
            sha256: digest,
            executable: true,
        }];
        let manifest = ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: identity.to_string(),
            install_generation: 7,
            channel: "stable".to_string(),
            minimum_version: env!("CARGO_PKG_VERSION").to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            files,
            rollout: RolloutMetadata {
                cohort: "all".to_string(),
                percentage: 100,
            },
        };
        let signing_key = SigningKey::from_bytes(&[9; 32]);
        let signed = sign_release_manifest(manifest, &signing_key).expect("signed fixture");
        verify_release_manifest(
            &signed,
            &signing_key.verifying_key(),
            &ReleasePolicy::native(0),
        )
        .expect("fixture signature verifies");
        let mut channel = ReleaseChannelPointer {
            schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
            channel: "stable".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            release_identity: identity.to_string(),
            install_generation: 7,
            version: env!("CARGO_PKG_VERSION").to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            manifest_url: format!("{release_root}/manifest.json"),
            signed_release: signed,
            installer: ReleaseInstallerMetadata {
                filename: format!("Axiusflow-Setup{suffix}"),
                url: format!("{release_root}/Axiusflow-Setup{suffix}"),
                size: 12,
                sha256_b64url: URL_SAFE_NO_PAD.encode([4_u8; 32]),
            },
        };
        validate_release_channel(&channel, base_url).expect("exact channel envelope");
        channel.manifest_url = format!("{base_url}/releases/{identity}/manifest.json");
        assert!(validate_release_channel(&channel, base_url).is_err());
    }

    #[test]
    fn bounded_download_resumes_exactly_and_rejects_excess_bytes() {
        let base = temporary_base("download-resume");
        let partial = base.join("artifact.part");
        fs::write(&partial, b"abc").expect("initial partial bytes");
        write_bounded_download(std::io::Cursor::new(b"def"), &partial, 3, 6, true)
            .expect("resume exact remainder");
        assert_eq!(fs::read(&partial).expect("resumed bytes"), b"abcdef");
        assert!(
            write_bounded_download(std::io::Cursor::new(b"toolong"), &partial, 3, 6, true).is_err()
        );
        fs::remove_dir_all(base).expect("remove download fixture");
    }

    #[test]
    fn update_check_report_uses_generation_for_ordering_and_semver_for_presentation() {
        let available = update_check_report(7, 8, "0.3.0");
        assert_eq!(available.current_generation, 7);
        assert_eq!(available.latest_generation, 8);
        assert_eq!(available.current_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(available.latest_version, "0.3.0");
        assert!(available.update_available);

        let current = update_check_report(8, 8, env!("CARGO_PKG_VERSION"));
        assert!(!current.update_available);
    }

    #[test]
    fn publish_time_validation_rejects_impossible_dates() {
        assert!(valid_published_at("2026-09-07T00:00:00Z"));
        assert!(valid_published_at("2024-02-29T23:59:59.1Z"));
        assert!(!valid_published_at("2026-02-30T00:00:00Z"));
        assert!(!valid_published_at("2026-09-07T24:00:00Z"));
    }

    #[test]
    fn user_launch_failure_message_is_bounded() {
        let message = user_launch_error_message(&"x".repeat(800));
        assert!(message.starts_with("Axiusflow could not start.\n\n"));
        assert_eq!(message.chars().count(), 348);
    }

    // Note: the relocated-child path (`uninstall_from_root_with_key` with
    // `NativeHooks`) is proven by the physical installed-lifecycle campaign,
    // not here: it deletes real vault keys and data roots by design, which a
    // unit test must never trigger on a stateful machine.
}
