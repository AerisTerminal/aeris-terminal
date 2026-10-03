#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! Stable packaging launcher/updater. This binary lives outside version directories.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead as _, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use aeris_platform_runtime::{
    ActiveRelease, BLOCK_PLAN_FILENAME, BlockFilePlan, CredentialVault, InstallationInventory,
    LifecycleHooks, MAXIMUM_SIGNED_BLOCK_PLAN_BYTES, NativeCredentialVault,
    RELEASE_CHANNEL_SCHEMA_VERSION, ReleaseChannelPointer, ReleaseFile, ReleaseInstaller,
    ReleasePolicy, SignedBlockPlan, SignedReleaseManifest, VaultEntry, current_release_identity,
    decode_and_verify_block_plan, native_install_root, native_installation_inventory,
    replace_file_atomically, rollout_eligible, verify_release_file, verify_release_manifest,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::VerifyingKey;
use sha2::{Digest as _, Sha256};
use sysinfo::{ProcessesToUpdate, System};

// Candidate readiness is a bounded in-process runtime probe. Keep activation
// finite while allowing one complete cold-start attempt on slower machines.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const RESTART_COMMIT_TIMEOUT: Duration = Duration::from_secs(60);
// Stable launcher/desktop wire tokens. Continue emitting the pre-rename values
// for compatibility with installed desktops, while accepting the transient
// Aeris commit token emitted by rename-transition builds.
const UPDATE_RESTART_READY: &[u8] = b"AXIUSFLOW_UPDATE_RESTART_READY_V2\n";
const UPDATE_RESTART_COMMIT: &str = "AXIUSFLOW_UPDATE_RESTART_COMMIT_V1\n";
const TRANSITIONAL_UPDATE_RESTART_COMMIT: &str = "AERIS_UPDATE_RESTART_COMMIT_V1\n";
const MAXIMUM_UPDATE_RESTART_COMMIT_BYTES: usize = 128;
const MAXIMUM_INPUT_BYTES: u64 = 1024 * 1024;
const RELEASE_HTTP_TIMEOUT: Duration = Duration::from_mins(10);
const RELEASE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const RELEASE_CHANNEL: &str = "stable";
const PREPARED_UPDATE_FILE: &str = "prepared-update.json";
const QUARANTINED_RELEASE_FILE: &str = ".release-quarantine.json";
const MAXIMUM_QUARANTINE_BYTES: u64 = 4 * 1024;
const EARLY_DESKTOP_STARTUP_WINDOW: Duration = Duration::from_secs(5);
const EARLY_DESKTOP_POLL_INTERVAL: Duration = Duration::from_millis(25);

fn main() {
    let no_arguments = std::env::args_os().len() == 1;
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("Aeris lifecycle: {error}");
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
    const SCRIPT: &str = "$shell=New-Object -ComObject WScript.Shell; [void]$shell.Popup($env:AERIS_LAUNCH_ERROR,0,'Aeris Terminal',16)";
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
        .env("AERIS_LAUNCH_ERROR", message)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(target_os = "windows"))]
fn show_user_launch_error(_error: &str) {}

#[cfg(any(target_os = "windows", test))]
fn user_launch_error_message(error: &str) -> String {
    let detail: String = error.chars().take(320).collect();
    format!("Aeris Terminal could not start.\n\n{detail}")
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
    if command.as_deref() == Some("--launcher-identity") {
        require_no_more(arguments)?;
        let identity = current_release_identity();
        let encoded = serde_json::to_string(&LauncherIdentityReport {
            schema_version: 1,
            release_identity: identity.release_identity,
            install_generation: identity.install_generation,
        })
        .map_err(|_| "launcher identity could not be encoded".to_string())?;
        println!("{encoded}");
        return Ok(());
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
            launch_active_desktop(&installer, &hooks, &install_root)
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
        Some("--prepare-update") => {
            require_no_more(arguments)?;
            prepare_remote_update(&installer, &verifying_key, &install_root)
        }
        Some("--update-and-restart") => {
            require_no_more(arguments)?;
            update_and_restart(&installer, &verifying_key, &hooks, &install_root)
        }
        Some("--check-update") => {
            require_no_more(arguments)?;
            check_remote_update(&installer, &verifying_key, &install_root)
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
        _ => Err("usage: aeris_launcher <--launch-desktop|--install <manifest> <bundle>|--update|--prepare-update|--update-and-restart|--check-update|--recover|--remove-all-local-data|--promote-stable-launcher|--launcher-identity>".to_string()),
    }
}

#[derive(serde::Serialize)]
struct LauncherIdentityReport {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
}

fn bootstrap_update_and_launch(
    executable: &Path,
    verifying_key: &VerifyingKey,
) -> Result<(), String> {
    let install_root = native_install_root().map_err(|error| error.to_string())?;
    fs::create_dir_all(&install_root)
        .map_err(|_| "per-user Aeris installation root could not be created".to_string())?;
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
    // Normal startup only needs to know whether an installation exists before
    // deciding if a bootstrap download is required. The launch path below owns
    // the one full signature/hash/Authenticode audit before process creation.
    let active_release = installer
        .active_release()
        .map_err(|error| error.to_string())?;
    if bootstrap_requires_remote_install(active_release.as_ref()) {
        install_remote_update(&installer, verifying_key, &hooks, &install_root)?;
    }
    launch_active_desktop(&installer, &hooks, &install_root)
}

fn bootstrap_requires_remote_install(active: Option<&ActiveRelease>) -> bool {
    active.is_none()
}

#[cfg(target_os = "windows")]
fn windows_start_menu_shortcut() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("Microsoft/Windows/Start Menu/Programs/Aeris/Aeris.lnk"))
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
#[expect(
    clippy::unnecessary_wraps,
    reason = "shares the fallible Windows signature; only Windows creates a launcher shortcut"
)]
fn remove_launcher_registration() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn launcher_registration_absent() -> Result<bool, String> {
    Ok(!windows_start_menu_shortcut()?.exists())
}

#[cfg(not(target_os = "windows"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "shares the fallible Windows signature; only Windows creates a launcher shortcut"
)]
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
        ".aeris_launcher{}.next",
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
    install_root.join(format!("aeris_launcher{}", std::env::consts::EXE_SUFFIX))
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
        .ok_or_else(|| "no verified Aeris release is active".to_string())?;
    let expected = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!("aeris_launcher{}", std::env::consts::EXE_SUFFIX));
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

struct CheckedReleaseChannel {
    active: Option<ActiveRelease>,
    channel: ReleaseChannelPointer,
    offer_eligible: bool,
}

fn install_remote_update(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    hooks: &NativeHooks,
    install_root: &Path,
) -> Result<(), String> {
    let checked = checked_release_channel(installer, verifying_key, install_root)?;
    let signed = &checked.channel.signed_release;
    if reconcile_same_generation_launcher(
        checked.active.as_ref(),
        signed.manifest.install_generation,
        || spawn_active_launcher_promotion(installer),
    ) {
        return Ok(());
    }
    if !checked.offer_eligible {
        return Ok(());
    }
    let (downloads_root, bundle_root) = download_release_bundle(
        installer,
        checked.active.as_ref(),
        verifying_key,
        install_root,
        signed,
    )?;
    installer
        .install(signed, &bundle_root, hooks)
        .map_err(|error| error.to_string())?;
    if let Err(error) = spawn_active_launcher_promotion(installer) {
        eprintln!("Aeris Terminal launcher promotion deferred: {error}");
    }
    remove_download_bundle(&downloads_root, &bundle_root)?;
    Ok(())
}

fn prepare_remote_update(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    install_root: &Path,
) -> Result<(), String> {
    let mut checked = checked_release_channel(installer, verifying_key, install_root)?;
    let active = checked
        .active
        .as_ref()
        .ok_or_else(|| "no verified Aeris release is active".to_string())?;
    let current_generation = active.install_generation;
    if checked.channel.install_generation == current_generation || !checked.offer_eligible {
        print_update_check_report(&channel_update_check_report(
            current_generation,
            &checked.channel,
            checked.offer_eligible,
        ))?;
        return Ok(());
    }

    let (downloads_root, bundle_root) = match download_release_bundle(
        installer,
        checked.active.as_ref(),
        verifying_key,
        install_root,
        &checked.channel.signed_release,
    ) {
        Ok(downloaded) => downloaded,
        Err(first_error) => {
            let refreshed = checked_release_channel(installer, verifying_key, install_root)?;
            if !release_channel_target_changed(&checked.channel, &refreshed.channel) {
                return Err(first_error);
            }
            if refreshed.channel.install_generation == current_generation
                || !refreshed.offer_eligible
            {
                print_update_check_report(&channel_update_check_report(
                    current_generation,
                    &refreshed.channel,
                    refreshed.offer_eligible,
                ))?;
                return Ok(());
            }
            let downloaded = download_release_bundle(
                installer,
                refreshed.active.as_ref(),
                verifying_key,
                install_root,
                &refreshed.channel.signed_release,
            )?;
            checked = refreshed;
            downloaded
        }
    };
    write_prepared_update(&downloads_root, &checked.channel)?;
    cleanup_stale_downloads(&downloads_root, &bundle_root)?;
    print_update_check_report(&update_check_report(
        current_generation,
        checked.channel.install_generation,
        &checked.channel.version,
    ))
}

fn release_channel_target_changed(
    original: &ReleaseChannelPointer,
    refreshed: &ReleaseChannelPointer,
) -> bool {
    original.install_generation != refreshed.install_generation
        || original.release_identity != refreshed.release_identity
        || original.signed_release != refreshed.signed_release
}

fn download_release_bundle(
    installer: &ReleaseInstaller,
    active: Option<&ActiveRelease>,
    verifying_key: &VerifyingKey,
    install_root: &Path,
    signed: &SignedReleaseManifest,
) -> Result<(PathBuf, PathBuf), String> {
    let downloads_root = install_root.join(".release-downloads");
    prepare_secure_directory(&downloads_root)?;
    let bundle_root = downloads_root.join(format!(
        "{:020}-{}",
        signed.manifest.install_generation, signed.manifest.release_identity
    ));
    prepare_secure_directory(&bundle_root)?;
    let download_agent = release_http_agent(RELEASE_HTTP_TIMEOUT);
    let block_delivery = active.and_then(|active| {
        verified_block_delivery(installer, active, signed, verifying_key, &download_agent)
    });
    for file in &signed.manifest.files {
        if block_delivery.as_ref().is_some_and(|delivery| {
            delivery
                .signed_plan
                .plan
                .files
                .iter()
                .find(|plan| plan.path == file.path)
                .is_some_and(|plan| {
                    reconstruct_release_file(&download_agent, &bundle_root, file, delivery, plan)
                        .is_ok()
                })
        }) {
            continue;
        }
        download_release_file(&download_agent, &bundle_root, file)?;
        if let Ok(block_partial) = block_partial_download_path(&bundle_root.join(&file.path)) {
            let _ = fs::remove_file(block_partial);
        }
    }
    Ok((downloads_root, bundle_root))
}

fn remove_download_bundle(downloads_root: &Path, bundle_root: &Path) -> Result<(), String> {
    fs::remove_dir_all(bundle_root)
        .map_err(|_| "release installed but its download cache could not be removed".to_string())?;
    let prepared = downloads_root.join(PREPARED_UPDATE_FILE);
    match fs::remove_file(&prepared) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err("release installed but its prepared update record remains".to_string());
        }
    }
    if fs::read_dir(downloads_root).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(downloads_root);
    }
    Ok(())
}

fn write_prepared_update(
    downloads_root: &Path,
    channel: &ReleaseChannelPointer,
) -> Result<(), String> {
    let encoded = serde_json::to_vec(channel)
        .map_err(|_| "prepared update metadata could not be encoded".to_string())?;
    if encoded.len() as u64 > MAXIMUM_INPUT_BYTES {
        return Err("prepared update metadata exceeds the size bound".to_string());
    }
    let prepared = downloads_root.join(PREPARED_UPDATE_FILE);
    let next = downloads_root.join(format!(".{PREPARED_UPDATE_FILE}.next"));
    match fs::remove_file(&next) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("prepared update staging metadata could not be reset".to_string()),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)
        .map_err(|_| "prepared update staging metadata could not be created".to_string())?;
    file.write_all(&encoded)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|_| "prepared update metadata could not be committed".to_string())?;
    drop(file);
    replace_file_atomically(&next, &prepared)
        .map_err(|_| "prepared update metadata could not be activated".to_string())
}

fn cleanup_stale_downloads(downloads_root: &Path, keep_bundle: &Path) -> Result<(), String> {
    for entry in fs::read_dir(downloads_root)
        .map_err(|_| "release download cache could not be enumerated".to_string())?
    {
        let entry = entry.map_err(|_| "release download cache entry is invalid".to_string())?;
        let path = entry.path();
        if path == keep_bundle || path == downloads_root.join(PREPARED_UPDATE_FILE) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| "release download cache entry is invalid".to_string())?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            fs::remove_file(&path)
                .map_err(|_| "stale release download cache could not be removed".to_string())?;
        } else if metadata.is_dir() {
            fs::remove_dir_all(&path)
                .map_err(|_| "stale release download cache could not be removed".to_string())?;
        } else {
            return Err("release download cache entry is invalid".to_string());
        }
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
        eprintln!("Aeris Terminal launcher promotion deferred: {error}");
    }
    true
}

fn update_and_restart(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    hooks: &NativeHooks,
    install_root: &Path,
) -> Result<(), String> {
    let (desktop, prepared) = preflight_update_restart(installer, verifying_key, install_root)?;
    let current = checked_release_channel(installer, verifying_key, install_root)?;
    if !prepared_matches_current_offer(&prepared.signed_release, &current) {
        return Err("prepared Aeris update is no longer current and eligible".to_string());
    }
    // READY reports only that the signed prepared release passed preflight.
    // The desktop sends COMMIT after account/workspace durability succeeds;
    // without that second message this helper must never observe exit as
    // authorization to mutate the installed release.
    announce_update_restart_ready()?;
    wait_for_update_restart_commit()?;
    wait_for_desktop_stop(&desktop)?;

    let update_result = (|| {
        // Recovery may run candidate health checks, so it must happen only
        // after the interactive desktop that requested the handoff is gone.
        installer
            .recover(hooks)
            .map_err(|error| error.to_string())?;
        installer
            .install(&prepared.signed_release, &prepared.bundle_root, hooks)
            .map_err(|error| error.to_string())?;
        remove_download_bundle(&prepared.downloads_root, &prepared.bundle_root)
    })();
    if let Err(update_error) = update_result {
        eprintln!("Aeris Terminal update deferred: {update_error}");
        // The desktop has already yielded ownership to this launcher. Recover
        // whatever transaction state is safely recoverable, then relaunch the
        // verified active release so a transient update failure does not make
        // the application disappear.
        let recovery_error = installer.recover(hooks).err();
        match installer.audit_active_release() {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(format!(
                    "{update_error}; no verified Aeris release is active{}",
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
    launch_active_desktop(installer, hooks, install_root)
}

struct PreparedUpdate {
    signed_release: SignedReleaseManifest,
    downloads_root: PathBuf,
    bundle_root: PathBuf,
}

fn prepared_matches_current_offer(
    prepared: &SignedReleaseManifest,
    current: &CheckedReleaseChannel,
) -> bool {
    current.offer_eligible && &current.channel.signed_release == prepared
}

fn preflight_update_restart(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    install_root: &Path,
) -> Result<(PathBuf, PreparedUpdate), String> {
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no verified Aeris release is active".to_string())?;
    let downloads_root = install_root.join(".release-downloads");
    require_existing_secure_directory(&downloads_root)?;
    let channel: ReleaseChannelPointer =
        read_bounded_json(&downloads_root.join(PREPARED_UPDATE_FILE))?;
    verify_prepared_channel(&channel, &active, verifying_key, install_root)?;
    let signed_release = channel.signed_release;
    let bundle_root = downloads_root.join(format!(
        "{:020}-{}",
        signed_release.manifest.install_generation, signed_release.manifest.release_identity
    ));
    require_existing_secure_directory(&bundle_root)?;
    verify_prepared_bundle(&bundle_root, &signed_release)?;
    let desktop = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!("aeris_desktop{}", std::env::consts::EXE_SUFFIX));
    let metadata = fs::symlink_metadata(&desktop)
        .map_err(|_| "active Aeris desktop is unavailable".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("active Aeris desktop is invalid".to_string());
    }
    Ok((
        desktop,
        PreparedUpdate {
            signed_release,
            downloads_root,
            bundle_root,
        },
    ))
}

fn verify_prepared_bundle(
    bundle_root: &Path,
    signed_release: &SignedReleaseManifest,
) -> Result<(), String> {
    for file in &signed_release.manifest.files {
        verify_release_file(&bundle_root.join(&file.path), file)
            .map_err(|_| "prepared update artifact verification failed".to_string())?;
    }
    Ok(())
}

fn require_existing_secure_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "prepared update directory is unavailable".to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("prepared update directory is invalid".to_string());
    }
    Ok(())
}

fn verify_prepared_channel(
    channel: &ReleaseChannelPointer,
    active: &ActiveRelease,
    verifying_key: &VerifyingKey,
    install_root: &Path,
) -> Result<(), String> {
    let minimum_generation =
        bootstrap_minimum_generation()?.max(active.install_generation.saturating_add(1));
    verify_prepared_channel_with_policy(
        channel,
        active,
        verifying_key,
        minimum_generation,
        embedded_release_base_url()?,
        install_root,
        read_quarantined_release(install_root)?.as_ref(),
    )
}

fn verify_prepared_channel_with_policy(
    channel: &ReleaseChannelPointer,
    active: &ActiveRelease,
    verifying_key: &VerifyingKey,
    minimum_generation: u64,
    base_url: &str,
    install_root: &Path,
    quarantined: Option<&QuarantinedRelease>,
) -> Result<(), String> {
    verify_release_manifest(
        &channel.signed_release,
        verifying_key,
        &ReleasePolicy::native(minimum_generation),
    )
    .map_err(|error| error.to_string())?;
    validate_release_channel(channel, base_url)?;
    let signed = &channel.signed_release;
    if signed.manifest.channel != RELEASE_CHANNEL {
        return Err("prepared release manifest does not match the stable channel".to_string());
    }
    if signed.manifest.install_generation <= active.install_generation {
        return Err("no newer prepared Aeris update is available".to_string());
    }
    if !release_offer_eligible(channel, Some(active), install_root, quarantined) {
        return Err("prepared Aeris update is not eligible for this installation".to_string());
    }
    Ok(())
}

fn announce_update_restart_ready() -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(UPDATE_RESTART_READY)
        .and_then(|()| stdout.flush())
        .map_err(|_| "update restart acknowledgement could not be written".to_string())
}

fn wait_for_update_restart_commit() -> Result<(), String> {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("aeris-update-restart-commit".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let result = read_update_restart_commit(stdin.lock());
            let _ = result_tx.send(result);
        })
        .map_err(|_| "update restart commit could not be monitored".to_string())?;
    match result_rx.recv_timeout(RESTART_COMMIT_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err("update restart commit timed out".to_string()),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("update restart commit failed".to_string())
        }
    }
}

fn read_update_restart_commit(input: impl std::io::BufRead) -> Result<(), String> {
    let mut line = String::new();
    let mut bounded = input.take((MAXIMUM_UPDATE_RESTART_COMMIT_BYTES + 1) as u64);
    let count = bounded
        .read_line(&mut line)
        .map_err(|_| "update restart commit could not be read".to_string())?;
    if count == 0
        || line.len() > MAXIMUM_UPDATE_RESTART_COMMIT_BYTES
        || (line != UPDATE_RESTART_COMMIT && line != TRANSITIONAL_UPDATE_RESTART_COMMIT)
    {
        return Err("update restart was not committed".to_string());
    }
    Ok(())
}

fn wait_for_desktop_stop(desktop: &Path) -> Result<(), String> {
    let deadline = Instant::now() + RESTART_WAIT_TIMEOUT;
    while process_is_running(desktop) {
        if Instant::now() >= deadline {
            return Err("active Aeris desktop did not close for restart".to_string());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct QuarantinedRelease {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
}

fn quarantine_path(install_root: &Path) -> PathBuf {
    install_root.join(QUARANTINED_RELEASE_FILE)
}

fn read_quarantined_release(install_root: &Path) -> Result<Option<QuarantinedRelease>, String> {
    let path = quarantine_path(install_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("release quarantine metadata is unavailable".to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAXIMUM_QUARANTINE_BYTES
    {
        return Err("release quarantine metadata is invalid".to_string());
    }
    let bytes =
        fs::read(path).map_err(|_| "release quarantine metadata is unreadable".to_string())?;
    let quarantine: QuarantinedRelease = serde_json::from_slice(&bytes)
        .map_err(|_| "release quarantine metadata is malformed".to_string())?;
    if quarantine.schema_version != 1
        || quarantine.install_generation == 0
        || !valid_release_identity(&quarantine.release_identity)
    {
        return Err("release quarantine metadata is invalid".to_string());
    }
    Ok(Some(quarantine))
}

fn write_quarantined_release(install_root: &Path, failed: &ActiveRelease) -> Result<(), String> {
    let quarantine = QuarantinedRelease {
        schema_version: 1,
        release_identity: failed.release_identity.clone(),
        install_generation: failed.install_generation,
    };
    let encoded = serde_json::to_vec(&quarantine)
        .map_err(|_| "release quarantine metadata could not be encoded".to_string())?;
    if encoded.len() as u64 > MAXIMUM_QUARANTINE_BYTES {
        return Err("release quarantine metadata exceeds the size bound".to_string());
    }
    let path = quarantine_path(install_root);
    let next = install_root.join(format!("{QUARANTINED_RELEASE_FILE}.next"));
    match fs::remove_file(&next) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("release quarantine staging metadata could not be reset".to_string()),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)
        .map_err(|_| "release quarantine staging metadata could not be created".to_string())?;
    file.write_all(&encoded)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|_| "release quarantine metadata could not be committed".to_string())?;
    drop(file);
    replace_file_atomically(&next, &path)
        .map_err(|_| "release quarantine metadata could not be activated".to_string())
}

fn release_offer_eligible(
    channel: &ReleaseChannelPointer,
    active: Option<&ActiveRelease>,
    install_root: &Path,
    quarantined: Option<&QuarantinedRelease>,
) -> bool {
    if let Some(active) = active
        && channel.install_generation <= active.install_generation
    {
        return true;
    }
    if quarantined.is_some_and(|failed| {
        failed.install_generation == channel.install_generation
            && failed.release_identity == channel.release_identity
    }) {
        return false;
    }
    rollout_eligible(&channel.signed_release.manifest.rollout, install_root)
}

fn checked_release_channel(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    install_root: &Path,
) -> Result<CheckedReleaseChannel, String> {
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
    if let Some(active) = &active
        && signed.manifest.install_generation < active.install_generation
    {
        return Err("release channel generation regressed below the active release".to_string());
    }
    let quarantined = read_quarantined_release(install_root)?;
    let offer_eligible = release_offer_eligible(
        &channel,
        active.as_ref(),
        install_root,
        quarantined.as_ref(),
    );
    Ok(CheckedReleaseChannel {
        active,
        channel,
        offer_eligible,
    })
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

fn current_update_check_report(current_generation: u64) -> UpdateCheckReport {
    update_check_report(
        current_generation,
        current_generation,
        env!("CARGO_PKG_VERSION"),
    )
}

fn channel_update_check_report(
    current_generation: u64,
    channel: &ReleaseChannelPointer,
    offer_eligible: bool,
) -> UpdateCheckReport {
    if !offer_eligible {
        return current_update_check_report(current_generation);
    }
    update_check_report(
        current_generation,
        channel.install_generation,
        &channel.version,
    )
}

fn check_remote_update(
    installer: &ReleaseInstaller,
    verifying_key: &VerifyingKey,
    install_root: &Path,
) -> Result<(), String> {
    let checked = checked_release_channel(installer, verifying_key, install_root)?;
    let current_generation = checked
        .active
        .as_ref()
        .map_or(0, |release| release.install_generation);
    let report =
        channel_update_check_report(current_generation, &checked.channel, checked.offer_eligible);
    print_update_check_report(&report)
}

fn print_update_check_report(report: &UpdateCheckReport) -> Result<(), String> {
    let encoded = serde_json::to_string(&report)
        .map_err(|_| "update status could not be encoded".to_string())?;
    println!("{encoded}");
    Ok(())
}

fn spawn_active_launcher_promotion(installer: &ReleaseInstaller) -> Result<(), String> {
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no verified Aeris release is active".to_string())?;
    let launcher = installer
        .release_directory(&active)
        .map_err(|error| error.to_string())?
        .join(format!("aeris_launcher{}", std::env::consts::EXE_SUFFIX));
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
    let encoded = option_env!("AERIS_BOOTSTRAP_MIN_GENERATION").ok_or_else(|| {
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
    let url = option_env!("AERIS_RELEASE_BASE_URL")
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
    let expected_installer_name = format!("Aeris-Setup{}", std::env::consts::EXE_SUFFIX);
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

struct VerifiedBlockDelivery {
    signed_plan: SignedBlockPlan,
    source_manifest: SignedReleaseManifest,
    source_root: PathBuf,
}

fn verified_block_delivery(
    installer: &ReleaseInstaller,
    active: &ActiveRelease,
    target: &SignedReleaseManifest,
    verifying_key: &VerifyingKey,
    agent: &ureq::Agent,
) -> Option<VerifiedBlockDelivery> {
    let source_manifest = installer.verified_release_manifest(active).ok()?;
    let source_root = installer.release_directory(active).ok()?;
    let base_url = embedded_release_base_url().ok()?;
    let target_manifest = &target.manifest;
    let url = format!(
        "{base_url}/{}/{}/{}-{}/{}",
        target_manifest.platform,
        target_manifest.architecture,
        target_manifest.install_generation,
        target_manifest.release_identity,
        BLOCK_PLAN_FILENAME
    );
    let mut response = agent.get(&url).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAXIMUM_SIGNED_BLOCK_PLAN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAXIMUM_SIGNED_BLOCK_PLAN_BYTES {
        return None;
    }
    let signed_plan = decode_and_verify_block_plan(
        &bytes,
        &source_manifest.manifest,
        target_manifest,
        verifying_key,
    )
    .ok()?;
    Some(VerifiedBlockDelivery {
        signed_plan,
        source_manifest,
        source_root,
    })
}

fn reconstruct_release_file(
    agent: &ureq::Agent,
    bundle_root: &Path,
    target: &ReleaseFile,
    delivery: &VerifiedBlockDelivery,
    plan: &BlockFilePlan,
) -> Result<(), String> {
    reconstruct_release_file_with_fetch(
        bundle_root,
        target,
        &delivery.source_manifest,
        &delivery.source_root,
        plan,
        |target, offset, length| download_target_block(agent, target, offset, length),
    )
}

fn reconstruct_release_file_with_fetch(
    bundle_root: &Path,
    target: &ReleaseFile,
    source_manifest: &SignedReleaseManifest,
    source_root: &Path,
    plan: &BlockFilePlan,
    mut fetch: impl FnMut(&ReleaseFile, u64, u32) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let source = source_manifest
        .manifest
        .files
        .iter()
        .find(|file| file.path == plan.path)
        .ok_or_else(|| "release block source is unavailable".to_string())?;
    let source_path = source_root.join(&source.path);
    verify_release_file(&source_path, source)
        .map_err(|_| "release block source verification failed".to_string())?;

    let relative = Path::new(&target.path);
    prepare_release_parent(bundle_root, relative)?;
    let final_path = bundle_root.join(relative);
    if final_path.exists() {
        if verify_release_file(&final_path, target).is_ok() {
            return Ok(());
        }
        fs::remove_file(&final_path)
            .map_err(|_| "invalid cached release artifact could not be removed".to_string())?;
    }
    let partial_path = block_partial_download_path(&final_path)?;
    let (resume_index, mut target_offset) = resume_block_partial(&partial_path, plan)?;
    let mut source_file = File::open(&source_path)
        .map_err(|_| "release block source could not be opened".to_string())?;
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&partial_path)
        .map_err(|_| "release block partial artifact could not be opened".to_string())?;

    for block in plan.blocks.iter().skip(resume_index) {
        let bytes = if let Some(source_offset) = block.source_offset {
            read_source_block(&mut source_file, source_offset, block.length)?
        } else {
            fetch(target, target_offset, block.length)?
        };
        verify_block_payload(block, &bytes)?;
        output
            .write_all(&bytes)
            .map_err(|_| "release block partial artifact could not be written".to_string())?;
        target_offset = target_offset
            .checked_add(u64::from(block.length))
            .ok_or_else(|| "release block target offset overflowed".to_string())?;
    }
    output
        .flush()
        .and_then(|()| output.sync_all())
        .map_err(|_| "release block partial artifact could not be committed".to_string())?;
    drop(output);
    verify_release_file(&partial_path, target)
        .map_err(|_| "reconstructed release artifact hash verification failed".to_string())?;
    fs::rename(&partial_path, &final_path)
        .map_err(|_| "verified reconstructed release artifact could not be committed".to_string())
}

fn read_source_block(file: &mut File, offset: u64, length: u32) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "release block source could not be positioned".to_string())?;
    let mut bytes = vec![0_u8; length as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| "release block source ended unexpectedly".to_string())?;
    Ok(bytes)
}

fn download_target_block(
    agent: &ureq::Agent,
    target: &ReleaseFile,
    offset: u64,
    length: u32,
) -> Result<Vec<u8>, String> {
    if !valid_https_url(&target.url) || length == 0 {
        return Err("release block target is invalid".to_string());
    }
    let end = offset
        .checked_add(u64::from(length))
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| "release block target range overflowed".to_string())?;
    if end >= target.size {
        return Err("release block target range exceeds signed size".to_string());
    }
    let mut response = agent
        .get(&target.url)
        .header("Range", format!("bytes={offset}-{end}"))
        .call()
        .map_err(|_| "release block request failed".to_string())?;
    if response.status().as_u16() != 206 {
        return Err("release block server did not honor the exact range".to_string());
    }
    validate_closed_content_range(response.headers(), offset, end, target.size)?;
    let mut bytes = Vec::with_capacity(length as usize);
    response
        .body_mut()
        .as_reader()
        .take(u64::from(length) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "release block response could not be read".to_string())?;
    if bytes.len() != length as usize {
        return Err("release block response length is invalid".to_string());
    }
    Ok(bytes)
}

fn validate_closed_content_range(
    headers: &ureq::http::HeaderMap,
    start: u64,
    end: u64,
    total: u64,
) -> Result<(), String> {
    let value = headers
        .get("Content-Range")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "release block response omitted Content-Range".to_string())?;
    if value != format!("bytes {start}-{end}/{total}") {
        return Err("release block Content-Range does not match the signed artifact".to_string());
    }
    Ok(())
}

fn verify_block_payload(
    block: &aeris_platform_runtime::BlockDescriptor,
    bytes: &[u8],
) -> Result<(), String> {
    let expected = URL_SAFE_NO_PAD
        .decode(&block.sha256)
        .map_err(|_| "release block hash is invalid".to_string())?;
    if bytes.len() != block.length as usize
        || expected.len() != 32
        || expected.as_slice() != Sha256::digest(bytes).as_slice()
    {
        return Err("release block hash verification failed".to_string());
    }
    Ok(())
}

fn block_partial_download_path(final_path: &Path) -> Result<PathBuf, String> {
    let name = final_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "release artifact filename is invalid".to_string())?;
    Ok(final_path.with_file_name(format!("{name}.block.part")))
}

fn resume_block_partial(path: &Path, plan: &BlockFilePlan) -> Result<(usize, u64), String> {
    let length = partial_length(path, plan.target_size)?;
    if length == 0 {
        return Ok((0, 0));
    }
    let mut input = File::open(path)
        .map_err(|_| "release block partial artifact could not be read".to_string())?;
    let mut offset = 0_u64;
    let mut truncate_at = None;
    let mut resume_index = plan.blocks.len();
    for (index, block) in plan.blocks.iter().enumerate() {
        let end = offset
            .checked_add(u64::from(block.length))
            .ok_or_else(|| "release block partial offset overflowed".to_string())?;
        if length < end {
            truncate_at = Some(offset);
            resume_index = index;
            break;
        }
        let mut bytes = vec![0_u8; block.length as usize];
        input
            .read_exact(&mut bytes)
            .map_err(|_| "release block partial artifact ended unexpectedly".to_string())?;
        if verify_block_payload(block, &bytes).is_err() {
            truncate_at = Some(offset);
            resume_index = index;
            break;
        }
        offset = end;
    }
    drop(input);
    if let Some(truncate_at) = truncate_at {
        truncate_file_to(path, truncate_at)?;
        return Ok((resume_index, truncate_at));
    }
    if offset != length {
        truncate_file_to(path, offset)?;
    }
    Ok((resume_index, offset))
}

fn truncate_file_to(path: &Path, length: u64) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| {
            file.set_len(length)?;
            file.sync_all()
        })
        .map_err(|_| "release block partial artifact could not be reset".to_string())
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
    let staged = dir.join(format!("aeris_uninstall{}", std::env::consts::EXE_SUFFIX));
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
    let encoded = option_env!("AERIS_RELEASE_VERIFYING_KEY")
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EarlyDesktopStartup {
    Running,
    ExitedSuccessfully,
    ExitedUnsuccessfully,
}

fn launch_active_desktop(
    installer: &ReleaseInstaller,
    hooks: &NativeHooks,
    install_root: &Path,
) -> Result<(), String> {
    let active = installer
        .audit_active_release()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no verified Aeris release is active".to_string())?;
    let retained = installer
        .retained_known_good_release()
        .map_err(|error| error.to_string())?;
    let quarantined = match read_quarantined_release(install_root) {
        Ok(quarantined) => quarantined,
        Err(error) => {
            eprintln!("Aeris Terminal failed-release quarantine could not be read: {error}");
            None
        }
    };
    let launcher_generation = current_release_identity().install_generation;
    // A retained predecessor means this launch is still inside the bounded
    // rollback probation window. Do not let the candidate desktop spawn its
    // versioned launcher helper until that window has passed: on Windows the
    // helper executable would lock the very candidate directory rollback must
    // be able to remove.
    let suppress_launcher_promotion = retained.is_some()
        || suppress_launcher_promotion_for_generation(
            &active,
            quarantined.as_ref(),
            launcher_generation,
        );
    let mut child = match spawn_desktop_release(installer, &active, suppress_launcher_promotion) {
        Ok(child) => child,
        Err(error) if retained.is_some() => {
            eprintln!(
                "Aeris Terminal active release {} failed to start; restoring retained known-good release",
                active.install_generation
            );
            return rollback_and_launch_retained(installer, hooks, install_root, &active, &error);
        }
        Err(error) => return Err(error),
    };

    if retained.is_none() {
        return Ok(());
    }
    match observe_early_desktop_startup(&mut child, EARLY_DESKTOP_STARTUP_WINDOW) {
        Ok(EarlyDesktopStartup::ExitedUnsuccessfully) => {
            eprintln!(
                "Aeris Terminal active release {} exited during startup; restoring retained known-good release",
                active.install_generation
            );
            rollback_and_launch_retained(
                installer,
                hooks,
                install_root,
                &active,
                "active Aeris desktop exited during startup",
            )
        }
        Ok(EarlyDesktopStartup::Running | EarlyDesktopStartup::ExitedSuccessfully) => {
            if should_promote_after_probation(&active, launcher_generation)
                && let Err(error) = spawn_active_launcher_promotion(installer)
            {
                eprintln!("Aeris Terminal launcher promotion deferred: {error}");
            }
            Ok(())
        }
        Err(error) => {
            eprintln!("Aeris Terminal startup observation degraded: {error}");
            Ok(())
        }
    }
}

fn should_promote_after_probation(active: &ActiveRelease, launcher_generation: u64) -> bool {
    launcher_generation < active.install_generation
}

#[cfg(test)]
fn suppress_launcher_promotion(
    active: &ActiveRelease,
    quarantined: Option<&QuarantinedRelease>,
) -> bool {
    suppress_launcher_promotion_for_generation(
        active,
        quarantined,
        current_release_identity().install_generation,
    )
}

fn suppress_launcher_promotion_for_generation(
    active: &ActiveRelease,
    quarantined: Option<&QuarantinedRelease>,
    launcher_generation: u64,
) -> bool {
    launcher_generation > active.install_generation
        || quarantined.is_some_and(|failed| failed.install_generation > active.install_generation)
}

fn spawn_desktop_release(
    installer: &ReleaseInstaller,
    release: &ActiveRelease,
    suppress_launcher_promotion: bool,
) -> Result<std::process::Child, String> {
    let executable = installer
        .release_directory(release)
        .map_err(|error| error.to_string())?
        .join(format!("aeris_desktop{}", std::env::consts::EXE_SUFFIX));
    let mut command = Command::new(executable);
    if suppress_launcher_promotion {
        // Installed desktop defaults to workspace-tabs. Supplying the explicit
        // equivalent mode keeps a rolled-back desktop from running its
        // no-argument versioned-launcher promotion path.
        command.arg("--workspace-tabs");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "active Aeris process could not be started".to_string())
}

fn observe_early_desktop_startup(
    child: &mut std::process::Child,
    window: Duration,
) -> Result<EarlyDesktopStartup, String> {
    let deadline = Instant::now() + window;
    loop {
        match child.try_wait().map_err(redacted)? {
            Some(status) if status.success() => return Ok(EarlyDesktopStartup::ExitedSuccessfully),
            Some(_) => return Ok(EarlyDesktopStartup::ExitedUnsuccessfully),
            None if Instant::now() >= deadline => return Ok(EarlyDesktopStartup::Running),
            None => thread::sleep(EARLY_DESKTOP_POLL_INTERVAL),
        }
    }
}

fn rollback_and_launch_retained(
    installer: &ReleaseInstaller,
    hooks: &NativeHooks,
    install_root: &Path,
    failed: &ActiveRelease,
    startup_error: &str,
) -> Result<(), String> {
    // The rollback removes the failed generation. Persist its quarantine
    // identity first so a crash or immediate restart cannot reinstall the same
    // bad channel offer in a loop.
    write_quarantined_release(install_root, failed).map_err(|error| {
        format!("{startup_error}; failed release could not be quarantined: {error}")
    })?;
    let retained = installer
        .rollback_to_retained_known_good(hooks)
        .map_err(|error| {
            format!("{startup_error}; retained known-good rollback failed: {error}")
        })?;
    spawn_desktop_release(installer, &retained, true)
        .map(|_| ())
        .map_err(|error| format!("{startup_error}; retained known-good desktop failed: {error}"))
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
    services: DesktopServiceReadiness,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopServiceReadiness {
    market: ServiceReady,
    account: ServiceReady,
    trading: ServiceReady,
    context: ServiceReady,
}

#[derive(serde::Deserialize)]
#[serde(transparent)]
struct ServiceReady(bool);

impl NativeHooks {
    fn desktop(&self, release: &ActiveRelease) -> PathBuf {
        self.install_root
            .join("versions")
            .join(&release.directory_name)
            .join(format!("aeris_desktop{}", std::env::consts::EXE_SUFFIX))
    }

    fn readiness_report(&self) -> PathBuf {
        self.install_root
            .join(".release-readiness")
            .join("readiness-report.json")
    }

    fn remove_readiness_report(path: &Path) -> Result<(), String> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("candidate desktop readiness report could not be removed".to_string()),
        }
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
                    return Err("active Aeris desktop must close before update".to_string());
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        Ok(())
    }

    fn health_check(&self, candidate: &ActiveRelease) -> Result<(), String> {
        let report = self.readiness_report();
        let report_root = report
            .parent()
            .ok_or_else(|| "candidate desktop readiness directory is invalid".to_string())?;
        prepare_secure_directory(report_root)?;
        Self::remove_readiness_report(&report)?;
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
            if readiness.schema_version != 4
                || readiness.release_identity != candidate.release_identity
                || readiness.install_generation != candidate.install_generation
                || readiness.desktop_process_id == 0
                || readiness.provider_count == 0
                || !readiness.workspace_restored
                || !readiness.services.market.0
                || !readiness.services.account.0
                || !readiness.services.trading.0
                || !readiness.services.context.0
            {
                return Err("candidate desktop readiness report is invalid".to_string());
            }
            let _ = readiness.workspace_revision;
            Ok(())
        })();
        let cleanup = Self::remove_readiness_report(&report);
        if cleanup.is_ok() {
            let _ = fs::remove_dir(report_root);
        }
        match (result, cleanup) {
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn disable_registrations(&self, registrations: &[String]) -> Result<(), String> {
        if registrations
            .iter()
            .any(|entry| entry == "start-menu:Aeris")
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
            return Err("an Aeris Terminal process remains active".to_string());
        }
        if inventory
            .registrations
            .iter()
            .any(|entry| entry == "start-menu:Aeris")
            && !launcher_registration_absent()?
        {
            return Err("an Aeris Terminal Start Menu shortcut remains".to_string());
        }
        for entry in &inventory.vault_entries {
            let vault = NativeCredentialVault::new(&entry.service).map_err(redacted)?;
            if vault.load(&entry.key).map_err(redacted)?.is_some() {
                return Err("an Aeris Terminal vault entry remains".to_string());
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
    let desktop = format!("aeris_desktop{}", std::env::consts::EXE_SUFFIX);
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
    "native Aeris Terminal lifecycle operation failed".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_platform_runtime::{
        BLOCK_PLAN_BLOCK_BYTES, BlockDescriptor, RELEASE_MANIFEST_SCHEMA_VERSION, ReleaseFileRole,
        ReleaseInstallerMetadata, ReleaseManifest, RolloutMetadata, sign_release_manifest,
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

    fn prepared_channel_fixture(
        generation: u64,
        payload: &[u8],
    ) -> (SigningKey, ReleaseChannelPointer) {
        let base_url = "https://auth.aeris.test/releases";
        let identity = "0123456789abcdef0123456789abcdef01234567";
        let release_root = format!(
            "{base_url}/{}/{}/{generation}-{identity}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let suffix = std::env::consts::EXE_SUFFIX;
        let file = ReleaseFile {
            role: ReleaseFileRole::Desktop,
            path: format!("aeris_desktop{suffix}"),
            url: format!("{release_root}/aeris_desktop{suffix}"),
            size: payload.len() as u64,
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(payload)),
            executable: true,
        };
        let manifest = ReleaseManifest {
            schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
            release_identity: identity.to_string(),
            install_generation: generation,
            channel: "stable".to_string(),
            minimum_version: env!("CARGO_PKG_VERSION").to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            files: vec![file],
            rollout: RolloutMetadata {
                cohort: "all".to_string(),
                percentage: 100,
            },
        };
        let signing_key = SigningKey::from_bytes(&[9; 32]);
        let signed_release =
            sign_release_manifest(manifest, &signing_key).expect("signed prepared fixture");
        let channel = ReleaseChannelPointer {
            schema_version: RELEASE_CHANNEL_SCHEMA_VERSION,
            channel: "stable".to_string(),
            platform: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            release_identity: identity.to_string(),
            install_generation: generation,
            version: env!("CARGO_PKG_VERSION").to_string(),
            published_at: "2026-09-07T00:00:00Z".to_string(),
            manifest_url: format!("{release_root}/manifest.json"),
            signed_release,
            installer: ReleaseInstallerMetadata {
                filename: format!("Aeris-Setup{suffix}"),
                url: format!("{release_root}/Aeris-Setup{suffix}"),
                size: 12,
                sha256_b64url: URL_SAFE_NO_PAD.encode([4_u8; 32]),
            },
        };
        (signing_key, channel)
    }

    fn block_descriptor(bytes: &[u8], source_offset: Option<u64>) -> BlockDescriptor {
        BlockDescriptor {
            length: u32::try_from(bytes.len()).expect("fixture block fits u32"),
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)),
            source_offset,
        }
    }

    #[test]
    fn preparation_retry_only_follows_a_changed_authenticated_target() {
        let (_, original) = prepared_channel_fixture(7, b"original");
        assert!(!release_channel_target_changed(&original, &original));

        let (_, newer) = prepared_channel_fixture(8, b"newer");
        assert!(release_channel_target_changed(&original, &newer));

        let mut changed_same_generation = original.clone();
        changed_same_generation.signed_release = newer.signed_release;
        assert!(release_channel_target_changed(
            &original,
            &changed_same_generation
        ));
    }

    #[test]
    fn block_reconstruction_resumes_at_verified_boundary_and_produces_exact_target() {
        let base = temporary_base("block-reconstruct");
        let source_root = base.join("source");
        let bundle_root = base.join("bundle");
        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&bundle_root).expect("bundle root");
        let block = usize::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits usize");
        let first_source = vec![1_u8; block];
        let reused = vec![2_u8; block];
        let downloaded = vec![3_u8; block];
        let source_bytes = [first_source, reused.clone()].concat();
        let target_bytes = [reused.clone(), downloaded.clone()].concat();
        let (_, source_channel) = prepared_channel_fixture(7, &source_bytes);
        let source_manifest = source_channel.signed_release;
        let source = &source_manifest.manifest.files[0];
        fs::write(source_root.join(&source.path), &source_bytes).expect("source artifact");
        let target = ReleaseFile {
            size: target_bytes.len() as u64,
            sha256: URL_SAFE_NO_PAD.encode(Sha256::digest(&target_bytes)),
            url: "https://releases.aeris.test/target.exe".to_string(),
            ..source.clone()
        };
        let plan = BlockFilePlan {
            path: target.path.clone(),
            source_size: source.size,
            source_sha256: source.sha256.clone(),
            target_size: target.size,
            target_sha256: target.sha256.clone(),
            blocks: vec![
                block_descriptor(&reused, Some(BLOCK_PLAN_BLOCK_BYTES)),
                block_descriptor(&downloaded, None),
            ],
        };
        let final_path = bundle_root.join(&target.path);
        let partial = block_partial_download_path(&final_path).expect("block partial path");
        let mut interrupted = reused.clone();
        interrupted.extend_from_slice(b"partial");
        fs::write(&partial, interrupted).expect("interrupted partial");

        let mut fetched = Vec::new();
        reconstruct_release_file_with_fetch(
            &bundle_root,
            &target,
            &source_manifest,
            &source_root,
            &plan,
            |_, offset, length| {
                fetched.push((offset, length));
                Ok(downloaded.clone())
            },
        )
        .expect("reconstruction succeeds");
        assert_eq!(
            fetched,
            vec![(
                BLOCK_PLAN_BLOCK_BYTES,
                u32::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits u32"),
            )]
        );
        assert_eq!(fs::read(&final_path).expect("final artifact"), target_bytes);
        verify_release_file(&final_path, &target).expect("exact final hash verifies");
        assert!(!partial.exists());
        fs::remove_dir_all(base).expect("remove block fixture");
    }

    #[test]
    fn reconstructed_file_is_never_committed_when_final_signed_hash_mismatches() {
        let base = temporary_base("block-final-hash");
        let source_root = base.join("source");
        let bundle_root = base.join("bundle");
        fs::create_dir_all(&source_root).expect("source root");
        fs::create_dir_all(&bundle_root).expect("bundle root");
        let bytes =
            vec![7_u8; usize::try_from(BLOCK_PLAN_BLOCK_BYTES).expect("block size fits usize")];
        let (_, source_channel) = prepared_channel_fixture(7, &bytes);
        let source_manifest = source_channel.signed_release;
        let source = &source_manifest.manifest.files[0];
        fs::write(source_root.join(&source.path), &bytes).expect("source artifact");
        let target = ReleaseFile {
            sha256: URL_SAFE_NO_PAD.encode([99_u8; 32]),
            ..source.clone()
        };
        let plan = BlockFilePlan {
            path: target.path.clone(),
            source_size: source.size,
            source_sha256: source.sha256.clone(),
            target_size: target.size,
            target_sha256: target.sha256.clone(),
            blocks: vec![block_descriptor(&bytes, Some(0))],
        };
        let final_path = bundle_root.join(&target.path);
        assert!(
            reconstruct_release_file_with_fetch(
                &bundle_root,
                &target,
                &source_manifest,
                &source_root,
                &plan,
                |_, _, _| Err("range fetch must not be used".to_string()),
            )
            .is_err()
        );
        assert!(!final_path.exists());
        assert!(
            block_partial_download_path(&final_path)
                .expect("block partial path")
                .exists()
        );
        fs::remove_dir_all(base).expect("remove block fixture");
    }

    #[test]
    fn closed_block_range_validation_requires_exact_content_range() {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert(
            "Content-Range",
            ureq::http::HeaderValue::from_static("bytes 10-19/20"),
        );
        validate_closed_content_range(&headers, 10, 19, 20).expect("exact range accepted");
        assert!(validate_closed_content_range(&headers, 9, 19, 20).is_err());
        assert!(validate_closed_content_range(&headers, 10, 18, 20).is_err());
        assert!(validate_closed_content_range(&headers, 10, 19, 21).is_err());
    }

    #[test]
    fn verified_active_release_skips_bootstrap_remote_install() {
        let active = ActiveRelease {
            release_identity: "current".to_string(),
            install_generation: 7,
            directory_name: "00000000000000000007-current".to_string(),
        };
        assert!(!bootstrap_requires_remote_install(Some(&active)));
        assert!(bootstrap_requires_remote_install(None));
    }

    #[test]
    fn readiness_report_stays_outside_immutable_version_inventory() {
        let base = temporary_base("readiness-report");
        let install_root = base.join("install");
        let hooks = NativeHooks {
            install_root: install_root.clone(),
        };
        let report = hooks.readiness_report();
        assert_eq!(
            report,
            install_root
                .join(".release-readiness")
                .join("readiness-report.json")
        );
        assert!(!report.starts_with(install_root.join("versions")));
        fs::remove_dir_all(base).expect("remove readiness fixture");
    }

    #[test]
    fn prepared_update_requires_signed_newer_metadata_and_exact_cached_files() {
        let payload = b"prepared desktop";
        let (signing_key, channel) = prepared_channel_fixture(8, payload);
        let base = temporary_base("prepared-update");
        let active = ActiveRelease {
            release_identity: "current".to_string(),
            install_generation: 7,
            directory_name: "00000000000000000007-current".to_string(),
        };
        verify_prepared_channel_with_policy(
            &channel,
            &active,
            &signing_key.verifying_key(),
            8,
            "https://auth.aeris.test/releases",
            &base,
            None,
        )
        .expect("newer signed prepared release verifies");

        let same_generation = ActiveRelease {
            install_generation: 8,
            ..active.clone()
        };
        assert!(
            verify_prepared_channel_with_policy(
                &channel,
                &same_generation,
                &signing_key.verifying_key(),
                8,
                "https://auth.aeris.test/releases",
                &base,
                None,
            )
            .is_err()
        );

        let mut tampered = channel.clone();
        tampered.signed_release.manifest.install_generation = 9;
        assert!(
            verify_prepared_channel_with_policy(
                &tampered,
                &active,
                &signing_key.verifying_key(),
                8,
                "https://auth.aeris.test/releases",
                &base,
                None,
            )
            .is_err()
        );

        let downloads = base.join(".release-downloads");
        let bundle = downloads.join(format!(
            "{:020}-{}",
            channel.install_generation, channel.release_identity
        ));
        fs::create_dir_all(&bundle).expect("prepared bundle directory");
        let file = &channel.signed_release.manifest.files[0];
        fs::write(bundle.join(&file.path), payload).expect("prepared artifact writes");
        verify_prepared_bundle(&bundle, &channel.signed_release)
            .expect("exact cached artifact verifies");
        fs::write(bundle.join(&file.path), b"modified desktop").expect("artifact mutates");
        assert!(verify_prepared_bundle(&bundle, &channel.signed_release).is_err());

        write_prepared_update(&downloads, &channel).expect("prepared metadata commits");
        let persisted: ReleaseChannelPointer =
            read_bounded_json(&downloads.join(PREPARED_UPDATE_FILE))
                .expect("prepared metadata reads");
        assert_eq!(persisted, channel);
        fs::remove_dir_all(base).expect("prepared fixture removes");
    }

    #[test]
    fn rollout_hold_and_quarantine_present_newer_release_as_current() {
        let base = temporary_base("rollout-gate");
        let (signing_key, mut channel) = prepared_channel_fixture(8, b"desktop");
        let active = ActiveRelease {
            release_identity: "aaaaaaaaaaaaaaaa".to_string(),
            install_generation: 7,
            directory_name: "00000000000000000007-aaaaaaaaaaaaaaaa".to_string(),
        };

        channel.signed_release.manifest.rollout = RolloutMetadata {
            cohort: "hold".to_string(),
            percentage: 0,
        };
        channel.signed_release =
            sign_release_manifest(channel.signed_release.manifest.clone(), &signing_key)
                .expect("held rollout resigns");
        assert!(!release_offer_eligible(
            &channel,
            Some(&active),
            &base,
            None
        ));
        assert!(!release_offer_eligible(&channel, None, &base, None));
        let held = channel_update_check_report(active.install_generation, &channel, false);
        assert_eq!(held.latest_generation, active.install_generation);
        assert_eq!(held.latest_version, env!("CARGO_PKG_VERSION"));
        assert!(!held.update_available);
        assert!(
            verify_prepared_channel_with_policy(
                &channel,
                &active,
                &signing_key.verifying_key(),
                8,
                "https://auth.aeris.test/releases",
                &base,
                None,
            )
            .is_err()
        );

        channel.signed_release.manifest.rollout = RolloutMetadata {
            cohort: "canary".to_string(),
            percentage: 100,
        };
        channel.signed_release =
            sign_release_manifest(channel.signed_release.manifest.clone(), &signing_key)
                .expect("canary rollout resigns");
        assert!(release_offer_eligible(&channel, Some(&active), &base, None));

        let quarantined = QuarantinedRelease {
            schema_version: 1,
            release_identity: channel.release_identity.clone(),
            install_generation: channel.install_generation,
        };
        assert!(!release_offer_eligible(
            &channel,
            Some(&active),
            &base,
            Some(&quarantined),
        ));
        assert!(
            verify_prepared_channel_with_policy(
                &channel,
                &active,
                &signing_key.verifying_key(),
                8,
                "https://auth.aeris.test/releases",
                &base,
                Some(&quarantined),
            )
            .is_err()
        );

        let failed = ActiveRelease {
            release_identity: channel.release_identity.clone(),
            install_generation: channel.install_generation,
            directory_name: format!(
                "{:020}-{}",
                channel.install_generation, channel.release_identity
            ),
        };
        write_quarantined_release(&base, &failed).expect("quarantine writes");
        assert_eq!(
            read_quarantined_release(&base).expect("quarantine reads"),
            Some(quarantined.clone())
        );
        assert!(suppress_launcher_promotion(&active, Some(&quarantined)));
        assert!(suppress_launcher_promotion_for_generation(&active, None, 8));
        assert!(!suppress_launcher_promotion_for_generation(
            &active, None, 6
        ));
        let newer_active = ActiveRelease {
            install_generation: 9,
            ..active
        };
        assert!(!suppress_launcher_promotion(
            &newer_active,
            Some(&quarantined)
        ));
        assert!(should_promote_after_probation(&newer_active, 8));
        assert!(!should_promote_after_probation(&newer_active, 9));
        assert!(!should_promote_after_probation(&newer_active, 10));
        fs::remove_dir_all(base).expect("rollout fixture removes");
    }

    #[test]
    fn prepared_restart_requires_current_channel_to_still_offer_exact_candidate() {
        let (signing_key, prepared_channel) = prepared_channel_fixture(8, b"desktop");
        let prepared = prepared_channel.signed_release.clone();
        let active = ActiveRelease {
            release_identity: "aaaaaaaaaaaaaaaa".to_string(),
            install_generation: 7,
            directory_name: "00000000000000000007-aaaaaaaaaaaaaaaa".to_string(),
        };
        let current = CheckedReleaseChannel {
            active: Some(active.clone()),
            channel: prepared_channel.clone(),
            offer_eligible: true,
        };
        assert!(prepared_matches_current_offer(&prepared, &current));

        let held = CheckedReleaseChannel {
            offer_eligible: false,
            ..current
        };
        assert!(!prepared_matches_current_offer(&prepared, &held));

        let mut changed_same_identity = prepared_channel.clone();
        let mut changed_manifest = changed_same_identity.signed_release.manifest.clone();
        changed_manifest.rollout.percentage = 50;
        changed_same_identity.signed_release =
            sign_release_manifest(changed_manifest, &signing_key).expect("changed offer signs");
        let changed_same_identity = CheckedReleaseChannel {
            active: Some(active.clone()),
            channel: changed_same_identity,
            offer_eligible: true,
        };
        assert!(
            !prepared_matches_current_offer(&prepared, &changed_same_identity),
            "pre-ACK revalidation must match the exact signed manifest, not only generation and identity"
        );

        let (_, superseding_channel) = prepared_channel_fixture(9, b"newer desktop");
        let superseded = CheckedReleaseChannel {
            active: Some(active),
            channel: superseding_channel,
            offer_eligible: true,
        };
        assert!(!prepared_matches_current_offer(&prepared, &superseded));
    }

    #[test]
    fn restart_ready_requires_a_separate_exact_commit_before_install() {
        assert!(
            read_update_restart_commit(std::io::Cursor::new(UPDATE_RESTART_COMMIT.as_bytes()))
                .is_ok()
        );
        assert!(
            read_update_restart_commit(std::io::Cursor::new(
                TRANSITIONAL_UPDATE_RESTART_COMMIT.as_bytes()
            ))
            .is_ok()
        );
        for invalid in [
            b"".as_slice(),
            UPDATE_RESTART_READY,
            b"AXIUSFLOW_UPDATE_RESTART_COMMIT_V1".as_slice(),
            b"AXIUSFLOW_UPDATE_RESTART_COMMIT_V1\r\n".as_slice(),
            b"AXIUSFLOW_UPDATE_RESTART_COMMIT_V2\n".as_slice(),
        ] {
            assert!(read_update_restart_commit(std::io::Cursor::new(invalid)).is_err());
        }
    }

    #[cfg(target_os = "windows")]
    fn startup_exit_child(delay_milliseconds: u64, exit_code: i32) -> std::process::Child {
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "if ({delay_milliseconds} -gt 0) {{ Start-Sleep -Milliseconds {delay_milliseconds} }}; exit {exit_code}"
                ),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("startup child starts")
    }

    #[cfg(not(target_os = "windows"))]
    fn startup_exit_child(delay_milliseconds: u64, exit_code: i32) -> std::process::Child {
        let seconds = format!(
            "{}.{:03}",
            delay_milliseconds / 1000,
            delay_milliseconds % 1000
        );
        Command::new("sh")
            .args(["-c", &format!("sleep {seconds}; exit {exit_code}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("startup child starts")
    }

    #[test]
    fn early_nonzero_exit_is_detected_but_observation_is_bounded() {
        let mut failed = startup_exit_child(0, 7);
        assert_eq!(
            observe_early_desktop_startup(&mut failed, Duration::from_secs(5))
                .expect("failed startup observes"),
            EarlyDesktopStartup::ExitedUnsuccessfully
        );

        let mut still_starting = startup_exit_child(1_000, 7);
        assert_eq!(
            observe_early_desktop_startup(&mut still_starting, Duration::from_millis(50))
                .expect("bounded startup observes"),
            EarlyDesktopStartup::Running
        );
        let _ = still_starting.kill();
        let _ = still_starting.wait();
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
        let destination = base.join("aeris_launcher.exe");
        fs::write(&source, b"new-launcher").expect("source fixture");
        fs::write(&destination, b"old-launcher").expect("destination fixture");

        persist_stable_launcher(&source, &destination).expect("replace stable launcher");

        assert_eq!(
            fs::read(&destination).expect("committed launcher"),
            b"new-launcher"
        );
        assert!(!base.join(".aeris_launcher.exe.next").exists());
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
        let base_url = "https://auth.aeris.test/releases";
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
            path: format!("aeris_desktop{suffix}"),
            url: format!("{release_root}/aeris_desktop{suffix}"),
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
                filename: format!("Aeris-Setup{suffix}"),
                url: format!("{release_root}/Aeris-Setup{suffix}"),
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
        assert!(message.starts_with("Aeris Terminal could not start.\n\n"));
        assert_eq!(
            message.chars().count(),
            "Aeris Terminal could not start.\n\n".chars().count() + 320
        );
    }

    // Note: the relocated-child path (`uninstall_from_root_with_key` with
    // `NativeHooks`) is proven by the physical installed-lifecycle campaign,
    // not here: it deletes real vault keys and data roots by design, which a
    // unit test must never trigger on a stateful machine.
}
