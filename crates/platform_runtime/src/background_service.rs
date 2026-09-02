//! Native per-user background-service integration for the resident engine.

use crate::CapabilityAvailability;
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use sysinfo::{ProcessesToUpdate, System};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;

const SERVICE_NAME: &str = "Axiusflow Engine";
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Platform-owned process and per-user autostart boundary for `axiusflow_engine`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundService {
    executable: PathBuf,
    autostart_executable: PathBuf,
    autostart_argument: Option<&'static str>,
}

impl BackgroundService {
    /// Creates a service boundary for one installed engine executable.
    ///
    /// # Errors
    /// Returns an error unless the executable path is absolute and names a file.
    pub fn new(executable: impl Into<PathBuf>) -> Result<Self, BackgroundServiceError> {
        let executable = executable.into();
        if !executable.is_absolute() || executable.file_name().is_none() {
            return Err(BackgroundServiceError::InvalidExecutable);
        }
        let autostart_executable =
            inferred_launcher(&executable).unwrap_or_else(|| executable.clone());
        let autostart_argument = (autostart_executable != executable).then_some("--launch-engine");
        Ok(Self {
            executable,
            autostart_executable,
            autostart_argument,
        })
    }

    /// Reports whether this target has a native per-user autostart integration.
    #[must_use]
    pub const fn autostart_availability() -> CapabilityAvailability {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        return CapabilityAvailability::Available;

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        CapabilityAvailability::Unavailable
    }

    /// Starts the engine without attaching it to the caller's terminal window.
    ///
    /// # Errors
    /// Returns an error when the operating system cannot create the process.
    pub fn start(&self) -> Result<(), BackgroundServiceError> {
        let mut command = Command::new(&self.executable);
        configure_background_process(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|_| BackgroundServiceError::ProcessStart)
    }

    /// Returns whether a running process owns this exact installed executable path.
    #[must_use]
    pub fn is_running(&self) -> bool {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All);
        system
            .processes()
            .values()
            .any(|process| process.exe() == Some(self.executable.as_path()))
    }

    /// Runs the engine's authenticated non-starting shutdown command with a deadline.
    ///
    /// # Errors
    /// Returns an error when the helper cannot start, times out, or reports failure.
    pub fn request_shutdown(&self, timeout: Duration) -> Result<(), BackgroundServiceError> {
        let mut command = Command::new(&self.executable);
        configure_background_process(&mut command);
        let mut child = command
            .arg("--shutdown")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| BackgroundServiceError::ProcessStart)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(BackgroundServiceError::DeadlineOverflow)?;
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| BackgroundServiceError::ProcessWait)?
            {
                return successful_status(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BackgroundServiceError::ShutdownTimeout);
            }
            thread::sleep(SHUTDOWN_POLL_INTERVAL);
        }
    }

    /// Configures or removes engine startup for the current user's OS session.
    ///
    /// # Errors
    /// Returns an error when the native user-session integration cannot be changed.
    pub fn set_autostart(&self, enabled: bool) -> Result<(), BackgroundServiceError> {
        #[cfg(target_os = "windows")]
        return configure_windows_autostart(
            &self.autostart_executable,
            self.autostart_argument,
            enabled,
        );

        #[cfg(target_os = "linux")]
        return configure_linux_autostart(
            &self.autostart_executable,
            self.autostart_argument,
            enabled,
        );

        #[cfg(target_os = "macos")]
        return configure_macos_autostart(
            &self.autostart_executable,
            self.autostart_argument,
            enabled,
        );

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = enabled;
            Err(BackgroundServiceError::UnsupportedPlatform)
        }
    }

    /// Returns whether the exact installed engine is configured for user-session startup.
    ///
    /// # Errors
    /// Returns an error when the native configuration cannot be inspected.
    pub fn autostart_enabled(&self) -> Result<bool, BackgroundServiceError> {
        #[cfg(target_os = "windows")]
        return windows_autostart_enabled(&self.autostart_executable, self.autostart_argument);

        #[cfg(target_os = "linux")]
        return Ok(read_exact_file(
            &linux_autostart_path()?,
            &linux_desktop_entry(&self.autostart_executable, self.autostart_argument),
        ));

        #[cfg(target_os = "macos")]
        return Ok(read_exact_file(
            &macos_autostart_path()?,
            &macos_launch_agent(&self.autostart_executable, self.autostart_argument),
        ));

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(BackgroundServiceError::UnsupportedPlatform)
    }
}

fn inferred_launcher(engine: &Path) -> Option<PathBuf> {
    let versions = engine.parent()?.parent()?;
    (versions.file_name().is_some_and(|name| name == "versions")).then(|| {
        versions.parent().unwrap_or(versions).join(format!(
            "axiusflow_launcher{}",
            std::env::consts::EXE_SUFFIX
        ))
    })
}

fn successful_status(status: ExitStatus) -> Result<(), BackgroundServiceError> {
    if status.success() {
        Ok(())
    } else {
        Err(BackgroundServiceError::ShutdownRejected)
    }
}

#[cfg(target_os = "windows")]
fn configure_background_process(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_background_process(_command: &mut Command) {}

#[cfg(target_os = "windows")]
fn configure_windows_autostart(
    executable: &Path,
    argument: Option<&str>,
    enabled: bool,
) -> Result<(), BackgroundServiceError> {
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let mut command = Command::new("reg.exe");
    configure_background_process(&mut command);
    if enabled {
        command.args(["ADD", RUN_KEY, "/v", SERVICE_NAME, "/t", "REG_SZ", "/d"]);
        command.arg(quoted_windows_command(executable, argument));
        command.arg("/f");
    } else {
        command.args(["DELETE", RUN_KEY, "/v", SERVICE_NAME, "/f"]);
    }
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| BackgroundServiceError::AutostartUpdate)?;
    if status.success() || (!enabled && !windows_autostart_enabled(executable, argument)?) {
        Ok(())
    } else {
        Err(BackgroundServiceError::AutostartUpdate)
    }
}

#[cfg(target_os = "windows")]
fn windows_autostart_enabled(
    executable: &Path,
    argument: Option<&str>,
) -> Result<bool, BackgroundServiceError> {
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let output = Command::new("reg.exe")
        .args(["QUERY", RUN_KEY, "/v", SERVICE_NAME])
        .output()
        .map_err(|_| BackgroundServiceError::AutostartInspection)?;
    if !output.status.success() {
        return Ok(false);
    }
    let expected = quoted_windows_command(executable, argument);
    Ok(String::from_utf8_lossy(&output.stdout).contains(&expected))
}

#[cfg(any(target_os = "windows", test))]
fn quoted_windows_command(executable: &Path, argument: Option<&str>) -> String {
    argument.map_or_else(
        || format!("\"{}\"", executable.display()),
        |argument| format!("\"{}\" {argument}", executable.display()),
    )
}

#[cfg(target_os = "linux")]
fn configure_linux_autostart(
    executable: &Path,
    argument: Option<&str>,
    enabled: bool,
) -> Result<(), BackgroundServiceError> {
    configure_autostart_file(
        &linux_autostart_path()?,
        enabled,
        &linux_desktop_entry(executable, argument),
    )
}

#[cfg(any(target_os = "linux", test))]
fn linux_desktop_entry(executable: &Path, argument: Option<&str>) -> String {
    let argument = argument.map_or(String::new(), |argument| format!(" {argument}"));
    format!(
        "[Desktop Entry]\nType=Application\nName={SERVICE_NAME}\nExec={}{argument}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec_argument(executable),
    )
}

#[cfg(target_os = "linux")]
fn linux_autostart_path() -> Result<PathBuf, BackgroundServiceError> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".config")))
        .ok_or(BackgroundServiceError::UserConfigurationDirectory)?;
    Ok(config.join("autostart").join("axiusflow-engine.desktop"))
}

#[cfg(any(target_os = "linux", test))]
fn desktop_exec_argument(executable: &Path) -> String {
    let value = executable
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{value}\"")
}

#[cfg(target_os = "macos")]
fn configure_macos_autostart(
    executable: &Path,
    argument: Option<&str>,
    enabled: bool,
) -> Result<(), BackgroundServiceError> {
    configure_autostart_file(
        &macos_autostart_path()?,
        enabled,
        &macos_launch_agent(executable, argument),
    )
}

#[cfg(any(target_os = "macos", test))]
fn macos_launch_agent(executable: &Path, argument: Option<&str>) -> String {
    let argument = argument.map_or(String::new(), |argument| {
        format!("<string>{}</string>", xml_escape(argument))
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>com.axiusflow.engine</string><key>ProgramArguments</key><array><string>{}</string>{argument}</array><key>RunAtLoad</key><true/></dict></plist>\n",
        xml_escape(&executable.to_string_lossy()),
    )
}

#[cfg(target_os = "macos")]
fn macos_autostart_path() -> Result<PathBuf, BackgroundServiceError> {
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(BackgroundServiceError::UserConfigurationDirectory)?;
    Ok(root
        .join("Library")
        .join("LaunchAgents")
        .join("com.axiusflow.engine.plist"))
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn configure_autostart_file(
    path: &Path,
    enabled: bool,
    contents: &str,
) -> Result<(), BackgroundServiceError> {
    if !enabled {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(BackgroundServiceError::AutostartUpdate),
        };
    }
    let parent = path
        .parent()
        .ok_or(BackgroundServiceError::UserConfigurationDirectory)?;
    fs::create_dir_all(parent).map_err(|_| BackgroundServiceError::AutostartUpdate)?;
    fs::write(path, contents).map_err(|_| BackgroundServiceError::AutostartUpdate)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_exact_file(path: &Path, expected: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|contents| contents == expected)
}

/// Redacted native background-service failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundServiceError {
    InvalidExecutable,
    UnsupportedPlatform,
    UserConfigurationDirectory,
    AutostartInspection,
    AutostartUpdate,
    ProcessStart,
    ProcessWait,
    DeadlineOverflow,
    ShutdownTimeout,
    ShutdownRejected,
}

impl fmt::Display for BackgroundServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self {
            Self::InvalidExecutable => "background service executable path is invalid",
            Self::UnsupportedPlatform => "background service integration is unavailable",
            Self::UserConfigurationDirectory => "user configuration directory is unavailable",
            Self::AutostartInspection => "engine autostart configuration could not be inspected",
            Self::AutostartUpdate => "engine autostart configuration could not be changed",
            Self::ProcessStart => "background engine process could not be started",
            Self::ProcessWait => "background engine process status could not be observed",
            Self::DeadlineOverflow => "background service deadline overflowed",
            Self::ShutdownTimeout => "engine shutdown helper exceeded its deadline",
            Self::ShutdownRejected => "engine shutdown helper reported failure",
        };
        formatter.write_str(detail)
    }
}

impl Error for BackgroundServiceError {}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundService, desktop_exec_argument, linux_desktop_entry, macos_launch_agent,
        quoted_windows_command,
    };
    use std::path::{Path, PathBuf};

    fn absolute_engine() -> PathBuf {
        if cfg!(target_os = "windows") {
            PathBuf::from(r"C:\Program Files\Axiusflow\axiusflow_engine.exe")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/Applications/Axiusflow/axiusflow_engine")
        } else {
            PathBuf::from("/opt/axiusflow/axiusflow_engine")
        }
    }

    #[test]
    fn service_rejects_relative_executable_paths() {
        assert!(BackgroundService::new("axiusflow_engine").is_err());
        assert!(BackgroundService::new(absolute_engine()).is_ok());
    }

    #[test]
    fn service_detects_the_exact_current_process_executable() {
        let executable = std::env::current_exe().expect("current test executable");
        let service = BackgroundService::new(executable).expect("current service boundary");
        assert!(service.is_running());
    }

    #[test]
    fn platform_autostart_payloads_quote_the_exact_executable() {
        let executable = Path::new("/Program Files/Axiusflow & Co/engine \"quoted\"");
        assert!(quoted_windows_command(executable, None).starts_with('"'));
        assert!(desktop_exec_argument(executable).starts_with('"'));
        let desktop = linux_desktop_entry(executable, None);
        assert!(desktop.contains("Terminal=false"));
        assert!(desktop.contains("X-GNOME-Autostart-enabled=true"));
        let launch_agent = macos_launch_agent(executable, None);
        assert!(launch_agent.contains("Axiusflow &amp; Co"));
        assert!(launch_agent.contains("&quot;quoted&quot;"));
    }

    #[cfg(target_os = "windows")]
    fn versioned_engine_fixture() -> PathBuf {
        PathBuf::from(
            r"C:\Program Files\Axiusflow\versions\00000000000000000002-release-2\axiusflow_engine.exe",
        )
    }

    #[cfg(not(target_os = "windows"))]
    fn versioned_engine_fixture() -> PathBuf {
        PathBuf::from("/opt/axiusflow/versions/00000000000000000002-release-2/axiusflow_engine")
    }

    #[cfg(target_os = "windows")]
    fn stable_launcher_fixture() -> PathBuf {
        PathBuf::from(r"C:\Program Files\Axiusflow\axiusflow_launcher.exe")
    }

    #[cfg(not(target_os = "windows"))]
    fn stable_launcher_fixture() -> PathBuf {
        PathBuf::from("/opt/axiusflow/axiusflow_launcher")
    }

    #[test]
    fn versioned_engine_autostart_always_targets_the_stable_launcher() {
        let engine = versioned_engine_fixture();
        let service = BackgroundService::new(&engine).expect("versioned engine service");
        assert_eq!(service.autostart_executable, stable_launcher_fixture());
        assert_eq!(service.autostart_argument, Some("--launch-engine"));
        let launcher = stable_launcher_fixture();
        let launcher_display = launcher.display().to_string();
        assert!(
            !service
                .autostart_executable
                .display()
                .to_string()
                .contains("release-2")
        );
        if cfg!(target_os = "windows") {
            let command =
                quoted_windows_command(&service.autostart_executable, service.autostart_argument);
            assert!(command.contains(&launcher_display));
            assert!(command.ends_with("--launch-engine"));
            assert!(!command.contains("release-2"));
        } else {
            let desktop =
                linux_desktop_entry(&service.autostart_executable, service.autostart_argument);
            assert!(desktop.contains(&format!("{launcher_display}\" --launch-engine")));
            assert!(!desktop.contains("release-2"));
        }
    }
}
