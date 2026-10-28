//! Release fixture for the `lifecycle_e2e` test; it is never packaged for users. Copied into a
//! release as the desktop, it writes the readiness report the launcher asks for and records each
//! launch; copied as the launcher, it answers the promotion the launcher schedules after an
//! update. The release identity, which the version directory name carries, says how the desktop
//! behaves: `-reject-` fails readiness, `-crash-` exits with an error right after a normal
//! launch and `-hang-` keeps running without finishing its start; any other release records
//! the startup-complete marker and exits. Under any other file name it does nothing.
//!
//! The test copies this small program rather than itself: an executable that copies and runs
//! its own image is what antivirus heuristics report as self-replicating malware.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::Duration,
};

use aeris_release_lifecycle::{
    PRODUCTION_VAULT_NAMESPACE, READINESS_REPORT_SCHEMA_VERSION, ReadinessReport, ServiceReadiness,
    ServiceStarted, StartupComplete, record_startup_complete,
};

/// Events the installed roles record, one `<generation> <event>` line each, beside the
/// install root. The test reads the same file name.
const EVENTS_FILE: &str = "fixture-events.log";
/// How long a hanging desktop runs if the launcher never stops it, so a failed test leaves
/// no process behind for long.
const HANG_LIMIT: Duration = Duration::from_mins(5);

fn main() -> ExitCode {
    let Ok(executable) = std::env::current_exe() else {
        return ExitCode::from(2);
    };
    let role = executable
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    match role.as_str() {
        "aeris_desktop" => exit(desktop(&executable, &arguments)),
        "aeris_launcher" => exit(launcher(&executable, &arguments)),
        _ => ExitCode::from(2),
    }
}

fn exit(result: io::Result<u8>) -> ExitCode {
    result.map_or(ExitCode::from(2), ExitCode::from)
}

struct Installed {
    generation: u64,
    release_identity: String,
    install_root: PathBuf,
    events: PathBuf,
}

impl Installed {
    /// Reads the release from `<base>/<install root>/versions/<generation>-<identity>/<exe>`.
    fn locate(executable: &Path) -> io::Result<Self> {
        let invalid = || io::Error::other("the fixture is not inside a version directory");
        let version = executable.parent().ok_or_else(invalid)?;
        let (generation, release_identity) = version
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.split_once('-'))
            .ok_or_else(invalid)?;
        let install_root = version
            .parent()
            .and_then(Path::parent)
            .ok_or_else(invalid)?;
        let base = install_root.parent().ok_or_else(invalid)?;
        Ok(Self {
            generation: generation.parse().map_err(|_| invalid())?,
            release_identity: release_identity.to_string(),
            install_root: install_root.to_path_buf(),
            events: base.join(EVENTS_FILE),
        })
    }

    fn record(&self, event: &str) -> io::Result<()> {
        let mut events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events)?;
        writeln!(events, "{} {event}", self.generation)
    }
}

fn desktop(executable: &Path, arguments: &[OsString]) -> io::Result<u8> {
    let installed = Installed::locate(executable)?;
    if let [flag, report] = arguments
        && flag == "--desktop-readiness"
    {
        installed.record("readiness")?;
        let accepted = !installed.release_identity.contains("-reject-");
        let report_bytes = ReadinessReport {
            schema_version: READINESS_REPORT_SCHEMA_VERSION,
            release_identity: installed.release_identity.clone(),
            install_generation: installed.generation,
            desktop_process_id: std::process::id(),
            workspace_revision: 1,
            // A rejected candidate reports no registered provider.
            provider_count: usize::from(accepted),
            workspace_restored: true,
            validation_mode: false,
            instance_lock: true,
            log_path: installed.events.display().to_string(),
            vault_namespace: PRODUCTION_VAULT_NAMESPACE.to_string(),
            services: ServiceReadiness {
                market: ServiceStarted(true),
                account: ServiceStarted(true),
                trading: ServiceStarted(true),
                context: ServiceStarted(true),
            },
        }
        .encode()
        .map_err(io::Error::other)?;
        fs::write(report, report_bytes)?;
        return Ok(0);
    }
    let launch: Vec<String> = arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    installed.record(format!("launch {}", launch.join(" ")).trim_end())?;
    if installed.release_identity.contains("-crash-") {
        return Ok(3);
    }
    if installed.release_identity.contains("-hang-") {
        thread::sleep(HANG_LIMIT);
        return Ok(4);
    }
    record_startup_complete(
        &installed.install_root,
        &StartupComplete::for_current_process(installed.generation, &installed.release_identity),
    )
    .map_err(io::Error::other)?;
    Ok(0)
}

fn launcher(executable: &Path, arguments: &[OsString]) -> io::Result<u8> {
    let installed = Installed::locate(executable)?;
    if arguments != ["--promote-stable-launcher"] {
        return Ok(2);
    }
    installed.record("promote")?;
    Ok(0)
}
