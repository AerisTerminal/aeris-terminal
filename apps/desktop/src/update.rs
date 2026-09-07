//! Desktop presentation bridge for launcher-owned update discovery and restart.
//!
//! The stable launcher remains the only release-trust/network owner. This
//! module runs launcher process work on one bounded background worker and
//! exposes only sanitized presentation state to GPUI.

use std::{
    io::Read as _,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const MAXIMUM_CHECK_OUTPUT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    Current,
    Available { latest_generation: u64 },
    Error(String),
    PreparingRestart,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdatePresentation {
    pub system_version: String,
    pub state: UpdateState,
}

#[derive(Clone, Copy, Debug)]
enum UpdateRequest {
    Check,
    Restart,
}

#[derive(Debug)]
enum UpdateResult {
    Checked {
        system_version: String,
        result: Result<LauncherUpdateCheck, String>,
    },
    RestartPrepared(Result<(), String>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct LauncherUpdateCheck {
    schema_version: u32,
    current_generation: u64,
    latest_generation: u64,
    update_available: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UpdatePoll {
    pub changed: bool,
    pub restart_prepared: bool,
}

pub struct DesktopUpdater {
    requests: SyncSender<UpdateRequest>,
    results: Receiver<UpdateResult>,
    presentation: UpdatePresentation,
    request_pending: bool,
}

impl DesktopUpdater {
    pub fn new() -> Result<Self, String> {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("axiusflow-update-client".to_string())
            .spawn(move || run_update_worker(&request_rx, &result_tx))
            .map_err(|_| "update client could not start".to_string())?;
        Ok(Self {
            requests: request_tx,
            results: result_rx,
            presentation: UpdatePresentation {
                system_version: "Detecting system…".to_string(),
                state: UpdateState::Idle,
            },
            request_pending: false,
        })
    }

    #[must_use]
    pub fn presentation(&self) -> &UpdatePresentation {
        &self.presentation
    }

    pub fn request_check(&mut self) -> Result<(), String> {
        if self.request_pending {
            return Ok(());
        }
        self.requests
            .try_send(UpdateRequest::Check)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "update check is already pending".to_string(),
                mpsc::TrySendError::Disconnected(_) => "update client is unavailable".to_string(),
            })?;
        self.request_pending = true;
        self.presentation.state = UpdateState::Checking;
        Ok(())
    }

    pub fn request_restart(&mut self) -> Result<(), String> {
        if self.request_pending {
            return Err("an update request is already pending".to_string());
        }
        if !matches!(self.presentation.state, UpdateState::Available { .. }) {
            return Err("no verified Axiusflow update is available".to_string());
        }
        self.requests
            .try_send(UpdateRequest::Restart)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "update restart is already pending".to_string(),
                mpsc::TrySendError::Disconnected(_) => "update client is unavailable".to_string(),
            })?;
        self.request_pending = true;
        self.presentation.state = UpdateState::PreparingRestart;
        Ok(())
    }

    pub fn poll(&mut self) -> UpdatePoll {
        let result = match self.results.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return UpdatePoll::default(),
            Err(mpsc::TryRecvError::Disconnected) => {
                if self.request_pending {
                    self.request_pending = false;
                    self.presentation.state =
                        UpdateState::Error("update client stopped unexpectedly".to_string());
                    return UpdatePoll {
                        changed: true,
                        restart_prepared: false,
                    };
                }
                return UpdatePoll::default();
            }
        };
        self.request_pending = false;
        match result {
            UpdateResult::Checked {
                system_version,
                result,
            } => {
                self.presentation.system_version = system_version;
                self.presentation.state = match result {
                    Ok(report) if report.update_available => UpdateState::Available {
                        latest_generation: report.latest_generation,
                    },
                    Ok(_) => UpdateState::Current,
                    Err(error) => UpdateState::Error(error),
                };
                UpdatePoll {
                    changed: true,
                    restart_prepared: false,
                }
            }
            UpdateResult::RestartPrepared(Ok(())) => UpdatePoll {
                changed: true,
                restart_prepared: true,
            },
            UpdateResult::RestartPrepared(Err(error)) => {
                self.presentation.state = UpdateState::Error(error);
                UpdatePoll {
                    changed: true,
                    restart_prepared: false,
                }
            }
        }
    }
}

fn run_update_worker(requests: &Receiver<UpdateRequest>, results: &SyncSender<UpdateResult>) {
    while let Ok(request) = requests.recv() {
        let result = match request {
            UpdateRequest::Check => UpdateResult::Checked {
                system_version: system_version(),
                result: run_launcher_check(),
            },
            UpdateRequest::Restart => UpdateResult::RestartPrepared(spawn_update_restart()),
        };
        if results.send(result).is_err() {
            return;
        }
    }
}

fn stable_launcher() -> Result<std::path::PathBuf, String> {
    let launcher = axiusflow_platform_runtime::native_install_root()
        .map_err(|error| error.to_string())?
        .join(format!(
            "axiusflow_launcher{}",
            std::env::consts::EXE_SUFFIX
        ));
    let metadata = std::fs::symlink_metadata(&launcher)
        .map_err(|_| "installed Axiusflow launcher is unavailable".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("installed Axiusflow launcher is invalid".to_string());
    }
    Ok(launcher)
}

fn launcher_command(launcher: &std::path::Path) -> Command {
    let mut command = Command::new(launcher);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn run_launcher_check() -> Result<LauncherUpdateCheck, String> {
    let launcher = stable_launcher()?;
    let mut child = launcher_command(&launcher)
        .arg("--check-update")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "update check could not start".to_string())?;
    let deadline = Instant::now() + CHECK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("update check timed out".to_string());
            }
            Err(_) => return Err("update check failed".to_string()),
        }
    };
    if !status.success() {
        return Err("updates could not be checked right now".to_string());
    }
    let mut output = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take((MAXIMUM_CHECK_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .map_err(|_| "update status could not be read".to_string())?;
    }
    if output.len() > MAXIMUM_CHECK_OUTPUT_BYTES {
        return Err("update status exceeded its size bound".to_string());
    }
    let report: LauncherUpdateCheck =
        serde_json::from_slice(&output).map_err(|_| "update status was invalid".to_string())?;
    validate_launcher_report(report)
}

fn validate_launcher_report(report: LauncherUpdateCheck) -> Result<LauncherUpdateCheck, String> {
    let current = axiusflow_platform_runtime::current_release_identity().install_generation;
    if report.schema_version != 1
        || report.current_generation != current
        || report.latest_generation < report.current_generation
        || report.update_available != (report.latest_generation > report.current_generation)
    {
        return Err("update status was invalid".to_string());
    }
    Ok(report)
}

fn spawn_update_restart() -> Result<(), String> {
    launcher_command(&stable_launcher()?)
        .arg("--update-and-restart")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "update restart could not start".to_string())
}

fn system_version() -> String {
    sysinfo::System::long_os_version()
        .or_else(sysinfo::System::name)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

#[cfg(test)]
mod tests {
    use super::{LauncherUpdateCheck, validate_launcher_report};

    #[test]
    fn launcher_report_requires_generation_consistency() {
        let current = axiusflow_platform_runtime::current_release_identity().install_generation;
        let valid = LauncherUpdateCheck {
            schema_version: 1,
            current_generation: current,
            latest_generation: current.saturating_add(1),
            update_available: true,
        };
        assert!(validate_launcher_report(valid.clone()).is_ok());
        assert!(
            validate_launcher_report(LauncherUpdateCheck {
                update_available: false,
                ..valid
            })
            .is_err()
        );

        assert!(
            validate_launcher_report(LauncherUpdateCheck {
                schema_version: 1,
                current_generation: current,
                latest_generation: current,
                update_available: false,
            })
            .is_ok()
        );

        if current > 0 {
            assert!(
                validate_launcher_report(LauncherUpdateCheck {
                    schema_version: 1,
                    current_generation: current,
                    latest_generation: current - 1,
                    update_available: false,
                })
                .is_err()
            );
        }
    }
}
