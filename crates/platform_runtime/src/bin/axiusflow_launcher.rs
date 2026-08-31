//! Stable packaging launcher/updater. This binary lives outside version directories.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use axiusflow_platform_runtime::{
    ActiveRelease, BackgroundService, CredentialVault, InstallationInventory, LifecycleHooks,
    NativeCredentialVault, ReleaseInstaller, ReleasePolicy, SignedReleaseManifest, VaultEntry,
    native_installation_inventory,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::VerifyingKey;
use sysinfo::{ProcessesToUpdate, System};

const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAXIMUM_INPUT_BYTES: u64 = 1024 * 1024;

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("Axiusflow lifecycle: {error}");
        std::process::exit(1);
    }
}

fn run(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(redacted)?;
    let install_root = executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "stable launcher installation root is unavailable".to_string())?;
    let verifying_key = embedded_verifying_key()?;
    let installer = ReleaseInstaller::new(&install_root, verifying_key, ReleasePolicy::native(0))
        .map_err(|error| error.to_string())?;
    let hooks = NativeHooks {
        install_root: install_root.clone(),
    };
    match arguments
        .next()
        .and_then(|argument| argument.into_string().ok())
        .as_deref()
    {
        Some("--launch-desktop") => {
            require_no_more(arguments)?;
            installer.recover(&hooks).map_err(|error| error.to_string())?;
            launch_active(&installer, "axiusflow_desktop")
        }
        Some("--launch-engine") => {
            require_no_more(arguments)?;
            installer.recover(&hooks).map_err(|error| error.to_string())?;
            launch_active(&installer, "axiusflow_engine")
        }
        Some("--install") => {
            let manifest_path = required_path(&mut arguments, "signed manifest")?;
            let bundle_root = required_path(&mut arguments, "release bundle")?;
            require_no_more(arguments)?;
            let signed: SignedReleaseManifest = read_bounded_json(&manifest_path)?;
            installer
                .recover(&hooks)
                .and_then(|()| installer.install(&signed, &bundle_root, &hooks).map(|_| ()))
                .map_err(|error| error.to_string())
        }
        Some("--recover") => {
            require_no_more(arguments)?;
            installer.recover(&hooks).map_err(|error| error.to_string())
        }
        Some("--remove-all-local-data") => {
            require_no_more(arguments)?;
            let inventory = native_installation_inventory(&install_root)
                .map_err(|error| error.to_string())?;
            installer
                .uninstall(&inventory, &hooks)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        _ => Err("usage: axiusflow_launcher <--launch-desktop|--launch-engine|--install <manifest> <bundle>|--recover|--remove-all-local-data>".to_string()),
    }
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
    engine_process_id: u32,
    workspace_revision: u64,
    provider_count: usize,
    authenticated_ipc_ready: bool,
    workspace_restored: bool,
    market_service_ready: bool,
}

impl NativeHooks {
    fn engine(&self, release: &ActiveRelease) -> PathBuf {
        self.install_root
            .join("versions")
            .join(&release.directory_name)
            .join(format!("axiusflow_engine{}", std::env::consts::EXE_SUFFIX))
    }

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

    fn wait_for_engine_stop(&self, candidate: &ActiveRelease) -> Result<(), String> {
        let service = BackgroundService::new(self.engine(candidate)).map_err(redacted)?;
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while service.is_running() {
            if Instant::now() >= deadline {
                return Err("candidate engine did not stop after readiness".to_string());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

impl LifecycleHooks for NativeHooks {
    fn prepare_activation(&self, previous: Option<&ActiveRelease>) -> Result<(), String> {
        if let Some(previous) = previous {
            let service = BackgroundService::new(self.engine(previous)).map_err(redacted)?;
            if service.is_running() {
                service
                    .request_shutdown(SHUTDOWN_TIMEOUT)
                    .map_err(redacted)?;
            }
            if process_is_running(&self.desktop(previous)) || service.is_running() {
                return Err("active Axiusflow processes must close before update".to_string());
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
            if readiness.schema_version != 1
                || readiness.release_identity != candidate.release_identity
                || readiness.install_generation != candidate.install_generation
                || readiness.engine_process_id == 0
                || readiness.provider_count == 0
                || !readiness.authenticated_ipc_ready
                || !readiness.workspace_restored
                || !readiness.market_service_ready
            {
                return Err("candidate desktop readiness report is invalid".to_string());
            }
            let _ = readiness.workspace_revision;
            self.wait_for_engine_stop(candidate)
        })();
        let _ = fs::remove_file(report);
        result
    }

    fn disable_registrations(&self, _registrations: &[String]) -> Result<(), String> {
        if let Some(active) = active_from_root(&self.install_root)? {
            BackgroundService::new(self.engine(&active))
                .and_then(|service| service.set_autostart(false))
                .map_err(redacted)?;
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
    system.refresh_processes(ProcessesToUpdate::All);
    system
        .processes()
        .values()
        .any(|process| process.exe() == Some(executable))
}

fn owned_process_is_running(install_root: &Path) -> bool {
    let versions = install_root.join("versions");
    let desktop = format!("axiusflow_desktop{}", std::env::consts::EXE_SUFFIX);
    let engine = format!("axiusflow_engine{}", std::env::consts::EXE_SUFFIX);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All);
    system.processes().values().any(|process| {
        process.exe().is_some_and(|executable| {
            executable.starts_with(&versions)
                && executable.file_name().is_some_and(|name| {
                    name == std::ffi::OsStr::new(&desktop) || name == std::ffi::OsStr::new(&engine)
                })
        })
    })
}

fn redacted<E>(_error: E) -> String {
    "native Axiusflow lifecycle operation failed".to_string()
}
