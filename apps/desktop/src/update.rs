//! Desktop presentation bridge for launcher-owned update discovery and restart.
//!
//! The stable launcher remains the only release-trust/network owner. This
//! module runs launcher process work on one bounded background worker and
//! exposes only sanitized presentation state to GPUI.

use std::{
    fs::File,
    io::{BufRead as _, Read as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const MAXIMUM_CHECK_OUTPUT_BYTES: usize = 16 * 1024;
const RESTART_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const RESTART_CHILD_STABILITY_WINDOW: Duration = Duration::from_millis(150);
const MAXIMUM_RESTART_ACK_BYTES: usize = 128;
const MAXIMUM_LAUNCHER_BYTES: u64 = 64 * 1024 * 1024;
const UPDATE_RESTART_READY: &str = "AXIUSFLOW_UPDATE_RESTART_READY_V1\n";

static UPDATE_RESTART_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

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
    RestartPrepared(Result<PreparedRestart, String>),
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
    prepared_restart: Option<PreparedRestart>,
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
            prepared_restart: None,
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
            UpdateResult::RestartPrepared(Ok(mut prepared)) => {
                if let Err(error) = prepared.require_running() {
                    self.presentation.state = UpdateState::Error(error);
                    return UpdatePoll {
                        changed: true,
                        restart_prepared: false,
                    };
                }
                self.prepared_restart = Some(prepared);
                UpdatePoll {
                    changed: true,
                    restart_prepared: true,
                }
            }
            UpdateResult::RestartPrepared(Err(error)) => {
                self.presentation.state = UpdateState::Error(error);
                UpdatePoll {
                    changed: true,
                    restart_prepared: false,
                }
            }
        }
    }

    /// Re-checks and commits the already-acknowledged launcher handoff
    /// immediately before global shutdown. A helper that has already exited
    /// is rejected and the desktop remains open.
    pub fn commit_restart(&mut self) -> Result<(), String> {
        let mut prepared = self
            .prepared_restart
            .take()
            .ok_or_else(|| "update restart is not prepared".to_string())?;
        if let Err(error) = prepared.require_running() {
            self.presentation.state = UpdateState::Error(error.clone());
            return Err(error);
        }
        prepared.commit();
        Ok(())
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

fn stable_launcher() -> Result<PathBuf, String> {
    let install_root =
        axiusflow_platform_runtime::native_install_root().map_err(|error| error.to_string())?;
    let launcher = install_root.join(format!(
        "axiusflow_launcher{}",
        std::env::consts::EXE_SUFFIX
    ));
    let metadata = std::fs::symlink_metadata(&launcher)
        .map_err(|_| "installed Axiusflow launcher is unavailable".to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAXIMUM_LAUNCHER_BYTES
    {
        return Err("installed Axiusflow launcher is invalid".to_string());
    }
    let release = axiusflow_platform_runtime::current_release_identity();
    if release.install_generation == 0
        || release.release_identity.is_empty()
        || release.release_identity.len() > 128
        || !release
            .release_identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("installed Axiusflow release identity is invalid".to_string());
    }
    let signed_launcher = install_root
        .join("versions")
        .join(format!(
            "{:020}-{}",
            release.install_generation, release.release_identity
        ))
        .join(format!(
            "axiusflow_launcher{}",
            std::env::consts::EXE_SUFFIX
        ));
    if !launcher_files_match(&launcher, &signed_launcher)? {
        return Err("installed Axiusflow launcher is not the active signed launcher".to_string());
    }
    Ok(launcher)
}

fn launcher_files_match(left: &Path, right: &Path) -> Result<bool, String> {
    let left_metadata = std::fs::symlink_metadata(left)
        .map_err(|_| "installed Axiusflow launcher is unavailable".to_string())?;
    let right_metadata = std::fs::symlink_metadata(right)
        .map_err(|_| "active signed Axiusflow launcher is unavailable".to_string())?;
    if !left_metadata.is_file()
        || left_metadata.file_type().is_symlink()
        || !right_metadata.is_file()
        || right_metadata.file_type().is_symlink()
        || left_metadata.len() == 0
        || left_metadata.len() != right_metadata.len()
        || left_metadata.len() > MAXIMUM_LAUNCHER_BYTES
    {
        return Ok(false);
    }
    let mut left = File::open(left)
        .map_err(|_| "installed Axiusflow launcher could not be verified".to_string())?;
    let mut right = File::open(right)
        .map_err(|_| "active signed Axiusflow launcher could not be verified".to_string())?;
    let mut left_buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut right_buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let left_count = left
            .read(&mut left_buffer)
            .map_err(|_| "installed Axiusflow launcher could not be verified".to_string())?;
        let right_count = right
            .read(&mut right_buffer)
            .map_err(|_| "active signed Axiusflow launcher could not be verified".to_string())?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
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

#[derive(Debug)]
struct RestartSlot {
    in_flight: &'static AtomicBool,
    release_on_drop: bool,
}

impl RestartSlot {
    fn claim(in_flight: &'static AtomicBool) -> Result<Self, String> {
        claim_restart_slot(in_flight)?;
        Ok(Self {
            in_flight,
            release_on_drop: true,
        })
    }

    fn commit(&mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for RestartSlot {
    fn drop(&mut self) {
        if self.release_on_drop {
            release_restart_slot(self.in_flight);
        }
    }
}

#[derive(Debug)]
struct PreparedRestart {
    child: Option<Child>,
    slot: RestartSlot,
    committed: bool,
}

impl PreparedRestart {
    fn require_running(&mut self) -> Result<(), String> {
        let Some(child) = self.child.as_mut() else {
            return Err("update restart helper is unavailable".to_string());
        };
        match child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(_)) | Err(_) => {
                Err("update restart helper stopped before shutdown".to_string())
            }
        }
    }

    fn commit(&mut self) {
        self.committed = true;
        self.slot.commit();
    }
}

impl Drop for PreparedRestart {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_update_restart() -> Result<PreparedRestart, String> {
    let slot = RestartSlot::claim(&UPDATE_RESTART_IN_FLIGHT)?;
    let result = (|| {
        let child = launcher_command(&stable_launcher()?)
            .arg("--update-and-restart")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "update restart could not start".to_string())?;
        wait_for_restart_ready(child)
    })();
    result.map(|child| PreparedRestart {
        child: Some(child),
        slot,
        committed: false,
    })
}

fn claim_restart_slot(in_flight: &AtomicBool) -> Result<(), String> {
    in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| "update restart is already pending".to_string())
}

fn release_restart_slot(in_flight: &AtomicBool) {
    in_flight.store(false, Ordering::Release);
}

fn validate_restart_acknowledgement(line: &str) -> Result<(), String> {
    if line == UPDATE_RESTART_READY {
        Ok(())
    } else {
        Err("update restart was not accepted".to_string())
    }
}

fn wait_for_restart_ready(mut child: Child) -> Result<Child, String> {
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("update restart acknowledgement is unavailable".to_string());
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("axiusflow-update-restart-ack".to_string())
        .spawn(move || {
            let mut line = String::new();
            let mut reader =
                std::io::BufReader::new(stdout).take((MAXIMUM_RESTART_ACK_BYTES + 1) as u64);
            let result = reader
                .read_line(&mut line)
                .map_err(|_| "update restart acknowledgement could not be read".to_string())
                .and_then(|count| {
                    if count == 0 || line.len() > MAXIMUM_RESTART_ACK_BYTES {
                        return Err("update restart was not accepted".to_string());
                    }
                    validate_restart_acknowledgement(&line)
                });
            let _ = ready_tx.send(result);
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("update restart acknowledgement could not be monitored".to_string());
    }

    match ready_rx.recv_timeout(RESTART_HANDSHAKE_TIMEOUT) {
        Ok(Ok(())) => {
            let deadline = Instant::now() + RESTART_CHILD_STABILITY_WINDOW;
            loop {
                match child.try_wait() {
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => return Ok(child),
                    Ok(Some(_)) | Err(_) => {
                        return Err("update restart helper stopped before shutdown".to_string());
                    }
                }
            }
        }
        Ok(Err(error)) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("update restart acknowledgement timed out".to_string())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("update restart acknowledgement failed".to_string())
        }
    }
}

fn system_version() -> String {
    sysinfo::System::long_os_version()
        .or_else(sysinfo::System::name)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicBool, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        LauncherUpdateCheck, PreparedRestart, RestartSlot, claim_restart_slot,
        launcher_files_match, release_restart_slot, validate_launcher_report,
        validate_restart_acknowledgement, wait_for_restart_ready,
    };

    #[cfg(target_os = "windows")]
    fn acknowledgement_child(exit_after_ack: bool) -> Child {
        let tail = if exit_after_ack {
            ""
        } else {
            "; Start-Sleep -Seconds 2"
        };
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!("[Console]::Out.Write(\"AXIUSFLOW_UPDATE_RESTART_READY_V1`n\"){tail}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("ack fixture starts")
    }

    #[cfg(not(target_os = "windows"))]
    fn acknowledgement_child(exit_after_ack: bool) -> Child {
        let tail = if exit_after_ack { "" } else { "; sleep 2" };
        Command::new("sh")
            .args([
                "-c",
                &format!("printf 'AXIUSFLOW_UPDATE_RESTART_READY_V1\\n'{tail}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("ack fixture starts")
    }

    fn temporary_base(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!("axiusflow-update-{name}-{nanos}"));
        fs::create_dir_all(&path).expect("temporary base");
        path
    }

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

    #[test]
    fn restart_single_flight_allows_exactly_one_owner() {
        let in_flight = AtomicBool::new(false);
        assert!(claim_restart_slot(&in_flight).is_ok());
        assert!(claim_restart_slot(&in_flight).is_err());
        release_restart_slot(&in_flight);
        assert!(claim_restart_slot(&in_flight).is_ok());
    }

    #[test]
    fn restart_acknowledgement_is_exact_and_bounded_by_the_reader() {
        assert!(validate_restart_acknowledgement(super::UPDATE_RESTART_READY).is_ok());
        for invalid in [
            "",
            "AXIUSFLOW_UPDATE_RESTART_READY_V1",
            "AXIUSFLOW_UPDATE_RESTART_READY_V1\r\n",
            "AXIUSFLOW_UPDATE_RESTART_READY_V2\n",
            "AXIUSFLOW_UPDATE_RESTART_READY_V1\nextra",
        ] {
            assert!(validate_restart_acknowledgement(invalid).is_err());
        }
    }

    #[test]
    fn restart_ack_is_rejected_when_helper_exits_immediately_after_it() {
        let child = acknowledgement_child(true);
        assert!(wait_for_restart_ready(child).is_err());
    }

    #[test]
    fn restart_ack_requires_a_living_handoff_owner() {
        let child = acknowledgement_child(false);
        let mut child = wait_for_restart_ready(child).expect("living helper is accepted");
        assert!(child.try_wait().expect("helper state reads").is_none());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn dropping_uncommitted_restart_releases_process_slot() {
        static SLOT: AtomicBool = AtomicBool::new(false);
        SLOT.store(false, Ordering::Release);
        {
            let prepared = PreparedRestart {
                child: None,
                slot: RestartSlot::claim(&SLOT).expect("slot claims"),
                committed: false,
            };
            assert!(SLOT.load(Ordering::Acquire));
            drop(prepared);
        }
        assert!(!SLOT.load(Ordering::Acquire));
        assert!(RestartSlot::claim(&SLOT).is_ok());
        SLOT.store(false, Ordering::Release);
    }

    #[test]
    fn stable_launcher_bytes_must_match_the_active_versioned_copy() {
        let base = temporary_base("launcher-match");
        let stable = base.join("stable.exe");
        let active = base.join("active.exe");
        fs::write(&stable, b"signed-launcher").expect("stable fixture writes");
        fs::write(&active, b"signed-launcher").expect("active fixture writes");
        assert!(launcher_files_match(&stable, &active).expect("matching launchers compare"));
        fs::write(&stable, b"changed-launcher").expect("stable fixture mutates");
        assert!(!launcher_files_match(&stable, &active).expect("mismatch compares"));
        fs::remove_dir_all(base).expect("temporary base removes");
    }
}
