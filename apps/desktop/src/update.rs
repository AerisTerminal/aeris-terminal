//! Desktop presentation bridge for launcher-owned update discovery and restart.
//!
//! The stable launcher remains the only release-trust/network owner. This
//! module runs launcher process work on one bounded background worker and
//! exposes only sanitized presentation state to GPUI.

use std::{
    fs::File,
    io::{BufRead as _, Read as _, Write as _},
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
const PREPARE_TIMEOUT: Duration = Duration::from_mins(30);
const PERIODIC_CHECK_INTERVAL: Duration = Duration::from_mins(30);
const CHECK_RETRY_INITIAL: Duration = Duration::from_mins(1);
const CHECK_RETRY_MAXIMUM: Duration = Duration::from_mins(30);
const MAXIMUM_CHECK_OUTPUT_BYTES: usize = 16 * 1024;
const RESTART_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const RESTART_CHILD_STABILITY_WINDOW: Duration = Duration::from_millis(150);
const MAXIMUM_RESTART_ACK_BYTES: usize = 128;
const MAXIMUM_LAUNCHER_BYTES: u64 = 64 * 1024 * 1024;
const UPDATE_RESTART_READY: &str = "AXIUSFLOW_UPDATE_RESTART_READY_V2\n";
const UPDATE_RESTART_COMMIT: &[u8] = b"AXIUSFLOW_UPDATE_RESTART_COMMIT_V1\n";

static UPDATE_RESTART_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
const LOCAL_PACKAGE_BUILD: bool = option_env!("AXIUSFLOW_LOCAL_PACKAGE").is_some();

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    Current,
    Downloading { latest_version: String },
    ReadyToRestart { latest_version: String },
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
    Prepared(Result<LauncherUpdateCheck, String>),
    RestartPrepared(Result<PreparedRestart, String>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct LauncherUpdateCheck {
    schema_version: u32,
    current_generation: u64,
    latest_generation: u64,
    current_version: String,
    latest_version: String,
    update_available: bool,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct LauncherIdentityReport {
    schema_version: u32,
    release_identity: String,
    install_generation: u64,
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
    next_check_at: Instant,
    check_retry_delay: Duration,
}

impl DesktopUpdater {
    pub fn new() -> Result<Self, String> {
        if LOCAL_PACKAGE_BUILD {
            return Err("update checking is unavailable for local package builds".to_string());
        }
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("axiusflow-update-client".to_string())
            .spawn(move || run_update_worker(&request_rx, &result_tx))
            .map_err(|_| "update client could not start".to_string())?;
        let mut updater = Self {
            requests: request_tx,
            results: result_rx,
            presentation: UpdatePresentation {
                system_version: "Detecting system…".to_string(),
                state: UpdateState::Idle,
            },
            request_pending: false,
            prepared_restart: None,
            next_check_at: Instant::now() + PERIODIC_CHECK_INTERVAL,
            check_retry_delay: CHECK_RETRY_INITIAL,
        };
        updater.request_check()?;
        Ok(updater)
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
        self.next_check_at = Instant::now() + PERIODIC_CHECK_INTERVAL;
        Ok(())
    }

    pub fn request_restart(&mut self) -> Result<(), String> {
        if self.request_pending {
            return Err("an update request is already pending".to_string());
        }
        if !matches!(self.presentation.state, UpdateState::ReadyToRestart { .. }) {
            return Err("no prepared Axiusflow update is ready to restart".to_string());
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
        let now = Instant::now();
        let scheduled = self.request_periodic_check_if_due(now);
        let result = match self.results.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                return UpdatePoll {
                    changed: scheduled,
                    restart_prepared: false,
                };
            }
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
        match result {
            UpdateResult::Checked {
                system_version,
                result,
            } => {
                self.presentation.system_version = system_version;
                self.presentation.state = match result {
                    Ok(report) if report.update_available => {
                        self.request_pending = true;
                        UpdateState::Downloading {
                            latest_version: report.latest_version,
                        }
                    }
                    Ok(_) => {
                        self.request_pending = false;
                        self.record_check_success(now);
                        UpdateState::Current
                    }
                    Err(error) => {
                        self.request_pending = false;
                        self.record_check_failure(now);
                        UpdateState::Error(error)
                    }
                };
                UpdatePoll {
                    changed: true,
                    restart_prepared: false,
                }
            }
            UpdateResult::Prepared(result) => {
                self.request_pending = false;
                self.presentation.state = match result {
                    Ok(report) if report.update_available => UpdateState::ReadyToRestart {
                        latest_version: report.latest_version,
                    },
                    Ok(_) => {
                        self.record_check_success(now);
                        UpdateState::Current
                    }
                    Err(error) => {
                        self.record_check_failure(now);
                        UpdateState::Error(error)
                    }
                };
                UpdatePoll {
                    changed: true,
                    restart_prepared: false,
                }
            }
            UpdateResult::RestartPrepared(Ok(prepared)) => {
                self.request_pending = false;
                self.prepared_restart = Some(prepared);
                UpdatePoll {
                    changed: true,
                    restart_prepared: true,
                }
            }
            UpdateResult::RestartPrepared(Err(error)) => {
                self.request_pending = false;
                self.presentation.state = UpdateState::Error(error);
                UpdatePoll {
                    changed: true,
                    restart_prepared: false,
                }
            }
        }
    }

    /// Sends the launcher's explicit post-durability commit immediately before
    /// global shutdown. A helper that has already exited or whose commit pipe
    /// cannot be written is rejected and the desktop remains open.
    pub fn commit_restart(&mut self) -> Result<(), String> {
        let mut prepared = self
            .prepared_restart
            .take()
            .ok_or_else(|| "update restart is not prepared".to_string())?;
        if let Err(error) = prepared.require_running() {
            self.presentation.state = UpdateState::Error(error.clone());
            self.prepared_restart = Some(prepared);
            return Err(error);
        }
        if let Err(error) = prepared.commit() {
            self.presentation.state = UpdateState::Error(error.clone());
            self.prepared_restart = Some(prepared);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn cancel_prepared_restart(&mut self, error: String) -> Option<PreparedRestart> {
        self.request_pending = false;
        self.presentation.state = UpdateState::Error(error);
        self.prepared_restart.take()
    }

    fn request_periodic_check_if_due(&mut self, now: Instant) -> bool {
        if self.request_pending
            || self.prepared_restart.is_some()
            || now < self.next_check_at
            || matches!(
                self.presentation.state,
                UpdateState::Downloading { .. }
                    | UpdateState::ReadyToRestart { .. }
                    | UpdateState::PreparingRestart
            )
        {
            return false;
        }
        match self.requests.try_send(UpdateRequest::Check) {
            Ok(()) => {
                self.request_pending = true;
                self.presentation.state = UpdateState::Checking;
                self.next_check_at = now + PERIODIC_CHECK_INTERVAL;
                true
            }
            Err(mpsc::TrySendError::Full(_)) => false,
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.presentation.state =
                    UpdateState::Error("update client is unavailable".to_string());
                self.record_check_failure(now);
                true
            }
        }
    }

    fn record_check_success(&mut self, now: Instant) {
        self.check_retry_delay = CHECK_RETRY_INITIAL;
        self.next_check_at = now + PERIODIC_CHECK_INTERVAL;
    }

    fn record_check_failure(&mut self, now: Instant) {
        self.next_check_at = now + self.check_retry_delay;
        self.check_retry_delay = self
            .check_retry_delay
            .saturating_mul(2)
            .min(CHECK_RETRY_MAXIMUM);
    }
}

fn run_update_worker(requests: &Receiver<UpdateRequest>, results: &SyncSender<UpdateResult>) {
    while let Ok(request) = requests.recv() {
        match request {
            UpdateRequest::Check => {
                let result = run_launcher_check();
                let prepare = result.as_ref().is_ok_and(|report| report.update_available);
                if results
                    .send(UpdateResult::Checked {
                        system_version: system_version(),
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                if prepare
                    && results
                        .send(UpdateResult::Prepared(run_launcher_prepare()))
                        .is_err()
                {
                    return;
                }
            }
            UpdateRequest::Restart => {
                if results
                    .send(UpdateResult::RestartPrepared(spawn_update_restart()))
                    .is_err()
                {
                    return;
                }
            }
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
    if !launcher_is_trusted(&launcher, &signed_launcher)? {
        return Err("installed Axiusflow launcher is not the active signed launcher".to_string());
    }
    Ok(launcher)
}

fn launcher_is_trusted(launcher: &Path, active_signed_launcher: &Path) -> Result<bool, String> {
    let active = axiusflow_platform_runtime::current_release_identity();
    launcher_is_trusted_with(launcher, active_signed_launcher, |path| {
        #[cfg(target_os = "windows")]
        {
            if axiusflow_platform_runtime::verify_windows_publisher_signature(path).is_err() {
                return false;
            }
            launcher_identity(path).is_ok_and(|launcher| {
                launcher.schema_version == 1
                    && launcher.install_generation > active.install_generation
                    && launcher.install_generation > 0
                    && !launcher.release_identity.is_empty()
                    && launcher.release_identity.len() <= 128
                    && launcher
                        .release_identity
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = path;
            false
        }
    })
}

fn launcher_is_trusted_with<F>(
    launcher: &Path,
    active_signed_launcher: &Path,
    verify_publisher: F,
) -> Result<bool, String>
where
    F: FnOnce(&Path) -> bool,
{
    if launcher_files_match(launcher, active_signed_launcher)? {
        return Ok(true);
    }
    Ok(verify_publisher(launcher))
}

#[cfg(target_os = "windows")]
fn launcher_identity(launcher: &Path) -> Result<LauncherIdentityReport, String> {
    let mut child = launcher_command(launcher)
        .arg("--launcher-identity")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "installed Axiusflow launcher identity could not be queried".to_string())?;
    let deadline = Instant::now() + CHECK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("installed Axiusflow launcher identity query timed out".to_string());
            }
            Err(_) => return Err("installed Axiusflow launcher identity query failed".to_string()),
        }
    };
    if !status.success() {
        return Err("installed Axiusflow launcher identity query was rejected".to_string());
    }
    let mut output = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(1025)
            .read_to_end(&mut output)
            .map_err(|_| "installed Axiusflow launcher identity could not be read".to_string())?;
    }
    if output.len() > 1024 {
        return Err("installed Axiusflow launcher identity exceeded its size bound".to_string());
    }
    serde_json::from_slice(&output)
        .map_err(|_| "installed Axiusflow launcher identity was invalid".to_string())
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
    run_launcher_report(
        "--check-update",
        CHECK_TIMEOUT,
        "update check could not start",
        "update check timed out",
        "updates could not be checked right now",
    )
}

fn run_launcher_prepare() -> Result<LauncherUpdateCheck, String> {
    run_launcher_report(
        "--prepare-update",
        PREPARE_TIMEOUT,
        "update download could not start",
        "update download timed out",
        "update could not be downloaded right now",
    )
}

fn run_launcher_report(
    argument: &str,
    timeout: Duration,
    start_error: &str,
    timeout_error: &str,
    status_error: &str,
) -> Result<LauncherUpdateCheck, String> {
    let launcher = stable_launcher()?;
    let mut child = launcher_command(&launcher)
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| start_error.to_string())?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timeout_error.to_string());
            }
            Err(_) => return Err("update launcher process failed".to_string()),
        }
    };
    if !status.success() {
        return Err(status_error.to_string());
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
    if report.schema_version != 2
        || report.current_generation != current
        || report.latest_generation < report.current_generation
        || report.current_version.is_empty()
        || report.current_version.len() > 64
        || report.latest_version.is_empty()
        || report.latest_version.len() > 64
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
pub(super) struct PreparedRestart {
    child: Option<Child>,
    slot: Option<RestartSlot>,
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

    fn commit(&mut self) -> Result<(), String> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| "update restart helper is unavailable".to_string())?;
        send_restart_commit(child)?;
        self.committed = true;
        if let Some(slot) = self.slot.as_mut() {
            slot.commit();
        }
        Ok(())
    }
}

impl Drop for PreparedRestart {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let slot = self.slot.take();
        if let Some(mut child) = self.child.take() {
            // Entity teardown may drop the updater on GPUI. Reaping an
            // uncommitted helper belongs to a detached process-cleanup worker;
            // closing this value on GPUI must never wait for process exit. Keep
            // the single-flight slot until that old helper has actually gone.
            let _ = thread::Builder::new()
                .name("axiusflow-update-restart-cleanup".to_string())
                .spawn(move || {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(slot);
                });
        }
    }
}

fn spawn_update_restart() -> Result<PreparedRestart, String> {
    let slot = RestartSlot::claim(&UPDATE_RESTART_IN_FLIGHT)?;
    let result = (|| {
        let child = launcher_command(&stable_launcher()?)
            .arg("--update-and-restart")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "update restart could not start".to_string())?;
        wait_for_restart_ready(child)
    })();
    result.map(|child| PreparedRestart {
        child: Some(child),
        slot: Some(slot),
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

fn send_restart_commit(child: &mut Child) -> Result<(), String> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "update restart commit channel is unavailable".to_string())?;
    stdin
        .write_all(UPDATE_RESTART_COMMIT)
        .map_err(|_| "update restart could not be committed".to_string())
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
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    use super::{
        CHECK_RETRY_INITIAL, CHECK_RETRY_MAXIMUM, DesktopUpdater, LauncherUpdateCheck,
        PERIODIC_CHECK_INTERVAL, PreparedRestart, RestartSlot, UpdatePresentation, UpdateRequest,
        UpdateResult, UpdateState, claim_restart_slot, launcher_files_match,
        launcher_is_trusted_with, release_restart_slot, send_restart_commit,
        validate_launcher_report, validate_restart_acknowledgement, wait_for_restart_ready,
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
                &format!("[Console]::Out.Write(\"AXIUSFLOW_UPDATE_RESTART_READY_V2`n\"){tail}"),
            ])
            .stdin(Stdio::piped())
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
                &format!("printf 'AXIUSFLOW_UPDATE_RESTART_READY_V2\\n'{tail}"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("ack fixture starts")
    }

    #[cfg(target_os = "windows")]
    fn commit_receiver_child() -> Child {
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "$line=[Console]::In.ReadLine(); if ($line -ceq 'AXIUSFLOW_UPDATE_RESTART_COMMIT_V1') { exit 0 } else { exit 7 }",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("commit receiver starts")
    }

    #[cfg(not(target_os = "windows"))]
    fn commit_receiver_child() -> Child {
        Command::new("sh")
            .args([
                "-c",
                "IFS= read -r line; [ \"$line\" = 'AXIUSFLOW_UPDATE_RESTART_COMMIT_V1' ]",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("commit receiver starts")
    }

    fn temporary_base(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!("axiusflow-update-{name}-{nanos}"));
        fs::create_dir_all(&path).expect("temporary base");
        path
    }

    fn update_report(update_available: bool) -> LauncherUpdateCheck {
        let current = axiusflow_platform_runtime::current_release_identity().install_generation;
        LauncherUpdateCheck {
            schema_version: 2,
            current_generation: current,
            latest_generation: if update_available {
                current.saturating_add(1)
            } else {
                current
            },
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: if update_available {
                "0.3.0".to_string()
            } else {
                env!("CARGO_PKG_VERSION").to_string()
            },
            update_available,
        }
    }

    #[test]
    fn checked_update_downloads_before_restart_becomes_available() {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let mut updater = DesktopUpdater {
            requests: request_tx,
            results: result_rx,
            presentation: UpdatePresentation {
                system_version: "Detecting system…".to_string(),
                state: UpdateState::Checking,
            },
            request_pending: true,
            prepared_restart: None,
            next_check_at: Instant::now() + PERIODIC_CHECK_INTERVAL,
            check_retry_delay: CHECK_RETRY_INITIAL,
        };
        let report = update_report(true);

        result_tx
            .send(UpdateResult::Checked {
                system_version: "Windows".to_string(),
                result: Ok(report.clone()),
            })
            .expect("checked result queues");
        assert!(updater.poll().changed);
        assert_eq!(
            updater.presentation.state,
            UpdateState::Downloading {
                latest_version: report.latest_version.clone()
            }
        );
        assert!(updater.request_pending);
        assert!(updater.request_restart().is_err());

        result_tx
            .send(UpdateResult::Prepared(Ok(report.clone())))
            .expect("prepared result queues");
        assert!(updater.poll().changed);
        assert_eq!(
            updater.presentation.state,
            UpdateState::ReadyToRestart {
                latest_version: report.latest_version
            }
        );
        assert!(!updater.request_pending);
        updater
            .request_restart()
            .expect("prepared update can request restart");
        assert!(matches!(
            request_rx.try_recv().expect("restart request queues"),
            UpdateRequest::Restart
        ));
        assert_eq!(updater.presentation.state, UpdateState::PreparingRestart);
    }

    #[test]
    fn prepared_refresh_can_resolve_to_current_without_restart() {
        let (request_tx, _request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let mut updater = DesktopUpdater {
            requests: request_tx,
            results: result_rx,
            presentation: UpdatePresentation {
                system_version: "Windows".to_string(),
                state: UpdateState::Downloading {
                    latest_version: "0.3.0".to_string(),
                },
            },
            request_pending: true,
            prepared_restart: None,
            next_check_at: Instant::now() + PERIODIC_CHECK_INTERVAL,
            check_retry_delay: CHECK_RETRY_INITIAL,
        };
        result_tx
            .send(UpdateResult::Prepared(Ok(update_report(false))))
            .expect("prepared current result queues");

        assert!(updater.poll().changed);
        assert_eq!(updater.presentation.state, UpdateState::Current);
        assert!(!updater.request_pending);
        assert!(updater.request_restart().is_err());
    }

    #[test]
    fn periodic_check_uses_existing_worker_and_failure_backoff_is_bounded() {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let mut updater = DesktopUpdater {
            requests: request_tx,
            results: result_rx,
            presentation: UpdatePresentation {
                system_version: "Windows".to_string(),
                state: UpdateState::Current,
            },
            request_pending: false,
            prepared_restart: None,
            next_check_at: Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .expect("one second is representable"),
            check_retry_delay: CHECK_RETRY_INITIAL,
        };

        let scheduled = updater.poll();
        assert!(scheduled.changed);
        assert!(updater.request_pending);
        assert_eq!(updater.presentation.state, UpdateState::Checking);
        assert!(matches!(
            request_rx.try_recv().expect("periodic check queues"),
            UpdateRequest::Check
        ));

        result_tx
            .send(UpdateResult::Checked {
                system_version: "Windows".to_string(),
                result: Err("temporary channel failure".to_string()),
            })
            .expect("failed check result queues");
        assert!(updater.poll().changed);
        assert!(!updater.request_pending);
        assert_eq!(updater.check_retry_delay, CHECK_RETRY_INITIAL * 2);
        assert!(updater.check_retry_delay <= CHECK_RETRY_MAXIMUM);
        assert!(updater.next_check_at > Instant::now());
    }

    #[test]
    fn launcher_report_requires_generation_consistency() {
        let current = axiusflow_platform_runtime::current_release_identity().install_generation;
        let valid = LauncherUpdateCheck {
            schema_version: 2,
            current_generation: current,
            latest_generation: current.saturating_add(1),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: "0.3.0".to_string(),
            update_available: true,
        };
        assert!(validate_launcher_report(valid.clone()).is_ok());
        assert!(
            validate_launcher_report(LauncherUpdateCheck {
                current_version: "9.9.9".to_string(),
                ..valid.clone()
            })
            .is_ok(),
            "a publisher-trusted newer stable launcher must remain usable after rollback across an app semver bump"
        );
        assert!(
            validate_launcher_report(LauncherUpdateCheck {
                update_available: false,
                ..valid
            })
            .is_err()
        );

        assert!(
            validate_launcher_report(LauncherUpdateCheck {
                schema_version: 2,
                current_generation: current,
                latest_generation: current,
                current_version: env!("CARGO_PKG_VERSION").to_string(),
                latest_version: env!("CARGO_PKG_VERSION").to_string(),
                update_available: false,
            })
            .is_ok()
        );

        if current > 0 {
            assert!(
                validate_launcher_report(LauncherUpdateCheck {
                    schema_version: 2,
                    current_generation: current,
                    latest_generation: current - 1,
                    current_version: env!("CARGO_PKG_VERSION").to_string(),
                    latest_version: env!("CARGO_PKG_VERSION").to_string(),
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
            "AXIUSFLOW_UPDATE_RESTART_READY_V2",
            "AXIUSFLOW_UPDATE_RESTART_READY_V2\r\n",
            "AXIUSFLOW_UPDATE_RESTART_READY_V1\n",
            "AXIUSFLOW_UPDATE_RESTART_READY_V2\nextra",
        ] {
            assert!(validate_restart_acknowledgement(invalid).is_err());
        }
    }

    #[test]
    fn restart_commit_sends_the_exact_post_durability_token() {
        let mut child = commit_receiver_child();
        send_restart_commit(&mut child).expect("commit token writes");
        assert!(child.wait().expect("commit receiver exits").success());
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
                slot: Some(RestartSlot::claim(&SLOT).expect("slot claims")),
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
    fn failed_restart_commit_preserves_cleanup_ownership() {
        static SLOT: AtomicBool = AtomicBool::new(false);
        SLOT.store(false, Ordering::Release);
        let (requests, _request_rx) = mpsc::sync_channel(1);
        let (result_tx, results) = mpsc::sync_channel(1);
        let mut updater = DesktopUpdater {
            requests,
            results,
            presentation: UpdatePresentation {
                system_version: "Windows".to_string(),
                state: UpdateState::PreparingRestart,
            },
            request_pending: true,
            prepared_restart: None,
            next_check_at: Instant::now() + PERIODIC_CHECK_INTERVAL,
            check_retry_delay: CHECK_RETRY_INITIAL,
        };
        result_tx
            .send(UpdateResult::RestartPrepared(Ok(PreparedRestart {
                child: None,
                slot: Some(RestartSlot::claim(&SLOT).expect("slot claims")),
                committed: false,
            })))
            .expect("prepared restart result queues");

        let poll = updater.poll();
        assert!(poll.restart_prepared);
        assert!(SLOT.load(Ordering::Acquire));
        let error = updater
            .commit_restart()
            .expect_err("missing helper cannot commit restart");
        assert!(
            SLOT.load(Ordering::Acquire),
            "commit failure must retain cleanup ownership instead of dropping it on GPUI"
        );
        let cleanup = updater
            .cancel_prepared_restart(error)
            .expect("failed commit retains cleanup ownership");
        assert!(SLOT.load(Ordering::Acquire));
        drop(cleanup);
        assert!(!SLOT.load(Ordering::Acquire));
    }

    #[test]
    fn cancelling_prepared_restart_returns_cleanup_ownership() {
        static SLOT: AtomicBool = AtomicBool::new(false);
        SLOT.store(false, Ordering::Release);
        let (requests, _request_rx) = mpsc::sync_channel(1);
        let (_result_tx, results) = mpsc::sync_channel(1);
        let mut updater = DesktopUpdater {
            requests,
            results,
            presentation: UpdatePresentation {
                system_version: "Windows".to_string(),
                state: UpdateState::PreparingRestart,
            },
            request_pending: false,
            prepared_restart: Some(PreparedRestart {
                child: None,
                slot: Some(RestartSlot::claim(&SLOT).expect("slot claims")),
                committed: false,
            }),
            next_check_at: Instant::now() + PERIODIC_CHECK_INTERVAL,
            check_retry_delay: CHECK_RETRY_INITIAL,
        };

        let cleanup = updater
            .cancel_prepared_restart("workspace durability failed".to_string())
            .expect("prepared restart returns cleanup ownership");
        assert_eq!(
            updater.presentation.state,
            UpdateState::Error("workspace durability failed".to_string())
        );
        assert!(SLOT.load(Ordering::Acquire));
        drop(cleanup);
        assert!(!SLOT.load(Ordering::Acquire));
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

    #[test]
    fn rollback_accepts_a_newer_publisher_signed_stable_launcher() {
        let base = temporary_base("launcher-rollback-trust");
        let stable = base.join("stable.exe");
        let rolled_back = base.join("rolled-back.exe");
        fs::write(&stable, b"newer-stable-launcher").expect("stable fixture writes");
        fs::write(&rolled_back, b"older-versioned-launcher").expect("active fixture writes");
        assert!(
            launcher_is_trusted_with(&stable, &rolled_back, |_| true)
                .expect("publisher trust fallback evaluates")
        );
        assert!(
            !launcher_is_trusted_with(&stable, &rolled_back, |_| false)
                .expect("publisher trust rejection evaluates")
        );
        fs::remove_dir_all(base).expect("temporary base removes");
    }
}
