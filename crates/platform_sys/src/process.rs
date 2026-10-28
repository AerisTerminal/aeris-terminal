//! Process observation by executable image.
//!
//! Callers identify the processes they own by image path. Waiting uses a
//! process handle on Windows (one enumeration, then a kernel wait) instead of
//! re-scanning every process on a timer.

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Poll interval for targets without a waitable process handle.
#[cfg(not(target_os = "windows"))]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One running process, opened when it was enumerated.
pub struct RunningProcess {
    id: u32,
    image: PathBuf,
    #[cfg(target_os = "windows")]
    handle: crate::windows::process::ProcessHandle,
}

impl RunningProcess {
    /// Operating-system process identifier.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Full executable image path reported by the operating system.
    #[must_use]
    pub fn image(&self) -> &Path {
        &self.image
    }

    /// Waits up to `timeout` for this process to exit.
    ///
    /// Returns `true` once the process has exited and `false` on timeout.
    ///
    /// # Errors
    /// Returns an operating-system error when the process cannot be waited on.
    pub fn wait_for_exit(&self, timeout: Duration) -> io::Result<bool> {
        #[cfg(target_os = "windows")]
        {
            self.handle.wait_for_exit(timeout)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let deadline = Instant::now() + timeout;
            loop {
                if image_path(self.id).as_deref() != Some(self.image.as_path()) {
                    return Ok(true);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(EXIT_POLL_INTERVAL.min(deadline - now));
            }
        }
    }
}

/// Opens every running process, other than the current one, whose executable
/// image satisfies `matches`.
///
/// # Errors
/// Returns an operating-system error when processes cannot be enumerated, or
/// `PermissionDenied` when a matching process cannot be opened for waiting.
pub fn running_processes(matches: impl Fn(&Path) -> bool) -> io::Result<Vec<RunningProcess>> {
    let current = std::process::id();
    let mut running = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let mut buffer = crate::windows::process::ImagePathBuffer::new();
        for id in crate::windows::process::process_ids()? {
            if id == current {
                continue;
            }
            let Some(opened) = crate::windows::process::open_process(id, &mut buffer)? else {
                continue;
            };
            if !matches(&opened.image) {
                continue;
            }
            let handle = opened.handle.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "a matching process cannot be opened for waiting",
                )
            })?;
            running.push(RunningProcess {
                id,
                image: opened.image,
                handle,
            });
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for id in process_ids()? {
            if id == current {
                continue;
            }
            if let Some(image) = image_path(id)
                && matches(&image)
            {
                running.push(RunningProcess { id, image });
            }
        }
    }
    Ok(running)
}

/// Whether any running process, other than the current one, has an
/// executable image that satisfies `matches`.
///
/// # Errors
/// Returns an operating-system error when processes cannot be enumerated.
pub fn any_process_running(matches: impl Fn(&Path) -> bool) -> io::Result<bool> {
    running_processes(matches).map(|running| !running.is_empty())
}

/// Waits up to `timeout` until no running process image satisfies `matches`.
///
/// Each round enumerates once and waits on the matching processes; a final
/// enumeration confirms that none started meanwhile. Returns `true` when none
/// remain and `false` on timeout.
///
/// # Errors
/// Returns an operating-system error when processes cannot be enumerated or
/// a matching process cannot be waited on.
pub fn wait_for_processes_to_exit(
    matches: impl Fn(&Path) -> bool,
    timeout: Duration,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let running = running_processes(&matches)?;
        if running.is_empty() {
            return Ok(true);
        }
        for process in &running {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !process.wait_for_exit(remaining)? {
                return Ok(false);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn process_ids() -> io::Result<Vec<u32>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Some(id) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        {
            ids.push(id);
        }
    }
    Ok(ids)
}

#[cfg(target_os = "linux")]
fn image_path(id: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{id}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn process_ids() -> io::Result<Vec<u32>> {
    crate::macos::process::process_ids()
}

#[cfg(target_os = "macos")]
fn image_path(id: u32) -> Option<PathBuf> {
    crate::macos::process::image_path(id)
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn process_ids() -> io::Result<Vec<u32>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process enumeration is unavailable on this platform",
    ))
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn image_path(_id: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        process::{Command, Stdio},
        time::Duration,
    };

    #[cfg(target_os = "windows")]
    const SLEEPER_IMAGE: &str = "powershell.exe";
    #[cfg(not(target_os = "windows"))]
    const SLEEPER_IMAGE: &str = "sleep";

    fn sleeper() -> Command {
        let mut command = Command::new(SLEEPER_IMAGE);
        #[cfg(target_os = "windows")]
        command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
        #[cfg(not(target_os = "windows"))]
        command.arg("30");
        command
    }

    fn is_sleeper(image: &Path) -> bool {
        image
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(SLEEPER_IMAGE))
    }

    #[test]
    fn a_spawned_child_is_found_by_image_and_its_exit_is_observed() {
        let mut child = sleeper()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleeper starts");
        let id = child.id();
        let running = super::running_processes(is_sleeper).expect("enumerate");
        let found = running
            .iter()
            .find(|process| process.id() == id)
            .expect("the child is enumerated");
        let image = found.image().to_path_buf();
        assert!(image.is_absolute(), "{image:?}");
        assert!(
            !found
                .wait_for_exit(Duration::from_millis(50))
                .expect("wait")
        );
        child.kill().expect("kill sleeper");
        child.wait().expect("reap sleeper");
        assert!(found.wait_for_exit(Duration::from_secs(5)).expect("wait"));
        assert!(
            !super::running_processes(|candidate| candidate == image)
                .expect("enumerate")
                .iter()
                .any(|process| process.id() == id)
        );
    }

    #[test]
    fn the_current_process_and_unmatched_images_are_never_reported() {
        let current = std::env::current_exe().expect("current executable");
        assert!(
            super::running_processes(|image| image == current)
                .expect("enumerate")
                .iter()
                .all(|process| process.id() != std::process::id())
        );
        let absent = Path::new("/aeris/no/such/image");
        assert!(!super::any_process_running(|image| image == absent).expect("enumerate"));
        assert!(
            super::wait_for_processes_to_exit(|image| image == absent, Duration::ZERO)
                .expect("wait")
        );
    }
}
