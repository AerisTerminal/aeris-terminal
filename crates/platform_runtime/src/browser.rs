//! System-browser opening for engine-supplied authentication URLs.
//!
//! The desktop sends `BeginLogin` over IPC and the engine replies with an
//! Axiusflow authentication URL. This boundary opens that URL in the user's
//! system browser. It performs process work, so callers must keep it off the
//! UI thread on background workers. It never handles credentials or tokens.
//!
//! Windows launches through a hidden PowerShell host whose only input is the
//! validated URL on stdin: the URL never appears in launcher arguments or
//! script text, so shell metacharacters (`&`, `|`, `;`, …) in the
//! authorization query reach the browser untouched instead of being parsed.

/// Maximum authorization URL length accepted before opening a browser.
pub const MAXIMUM_AUTHORIZATION_URL_BYTES: usize = 2048;

/// How long the launcher process may take before it is killed and reported.
#[cfg(target_os = "windows")]
const LAUNCHER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Fixed PowerShell launcher script. Reads one URL from stdin and opens it
/// with the shell's default-browser verb (the `UseShellExecute` equivalent).
/// The script is constant: the URL travels as process input, never as code.
#[cfg(target_os = "windows")]
const LAUNCHER_SCRIPT: &str = "$u = [Console]::In.ReadLine(); if ([string]::IsNullOrWhiteSpace($u)) { exit 3 }; Start-Process -FilePath $u.Trim()";

/// Builds the hidden launcher process. The URL is never an argument: it is
/// written to stdin by [`run_launcher`], so query separators survive intact.
#[cfg(target_os = "windows")]
fn launcher_command(program: &str, script: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    // CREATE_NO_WINDOW: no console window flashes on sign-in.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new(program);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// Runs one launcher to completion with a bounded wait. A missing binary, a
/// nonzero exit, or a hung host all report the same actionable failure; a
/// hung host is killed first so no hidden PowerShell outlives sign-in.
#[cfg(target_os = "windows")]
fn run_launcher(mut command: std::process::Command, url: &str) -> Result<(), String> {
    use std::io::Write as _;
    let mut child = command
        .spawn()
        .map_err(|_| "system browser could not be opened".to_string())?;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = writeln!(stdin, "{url}");
    }
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + LAUNCHER_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("system browser could not be opened".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(Some(_)) | Err(_) => {
                return Err("system browser could not be opened".to_string());
            }
        }
    }
}

/// Opens an engine-supplied authentication URL in the system browser.
///
/// Only `https` URLs are opened; anything else fails closed without spawning
/// a process.
///
/// # Errors
///
/// Returns an error when the URL is empty, too long, not `https`, or the
/// native browser launcher cannot start.
pub fn open_system_browser(url: &str) -> Result<(), String> {
    let url = validate_authorization_url(url)?;
    spawn_browser(url)
}

fn validate_authorization_url(url: &str) -> Result<&str, String> {
    if url.is_empty() || url.len() > MAXIMUM_AUTHORIZATION_URL_BYTES {
        return Err("authentication URL is invalid".to_string());
    }
    if url.starts_with("https://") && !url.contains([' ', '\n', '\r', '\t']) {
        Ok(url)
    } else {
        Err("authentication URL is invalid".to_string())
    }
}

fn spawn_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        run_launcher(launcher_command("powershell.exe", LAUNCHER_SCRIPT), url)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|_| "system browser could not be opened".to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|_| "system browser could not be opened".to_string())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = url;
        Err("system browser could not be opened".to_string())
    }
}

#[cfg(all(test, target_os = "windows"))]
mod launcher_tests {
    use super::{LAUNCHER_SCRIPT, launcher_command, run_launcher};

    /// The `cmd /C start` defect, pinned: `&` query separators must reach
    /// the browser as data. The launcher takes no URL argument at all, so
    /// no shell can split the authorization query.
    #[test]
    fn authorization_query_never_enters_launcher_argv() {
        let url = "https://auth.axiusflow.com/api/auth/oauth2/authorize?response_type=code&client_id=axiusflow-desktop&redirect_uri=http%3A%2F%2F127.0.0.1%3A43129%2Fcallback&scope=openid%20offline_access&state=abc%7Cdef&nonce=xyz";
        super::validate_authorization_url(url).expect("real authorization URL validates");
        let command = launcher_command("powershell.exe", LAUNCHER_SCRIPT);
        assert_eq!(command.get_program(), "powershell.exe");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-Command".to_string()));
        assert!(args.contains(&"Hidden".to_string()));
        assert!(
            args.iter().all(|arg| !arg.contains("https://")),
            "URL must travel on stdin, never as an argument: {args:?}"
        );
    }

    #[test]
    fn missing_launcher_reports_failure() {
        let command = launcher_command("axiusflow-definitely-missing-launcher", LAUNCHER_SCRIPT);
        assert_eq!(
            run_launcher(command, "https://auth.axiusflow.com/sign-in?x=1&y=2"),
            Err("system browser could not be opened".to_string())
        );
    }

    #[test]
    fn nonzero_launcher_exit_reports_failure() {
        let command = launcher_command("powershell.exe", "exit 7");
        assert_eq!(
            run_launcher(command, "https://auth.axiusflow.com/sign-in?x=1&y=2"),
            Err("system browser could not be opened".to_string())
        );
    }

    /// Exercises the real host plumbing (stdin delivery, bounded wait, exit
    /// status) without opening a browser: the stub echoes stdin and exits.
    #[test]
    fn launcher_plumbing_delivers_stdin_and_exits_zero() {
        let command = launcher_command(
            "powershell.exe",
            "[Console]::WriteLine([Console]::In.ReadLine())",
        );
        assert_eq!(
            run_launcher(command, "https://auth.axiusflow.com/sign-in?x=1&y=2"),
            Ok(())
        );
    }

    #[test]
    fn hung_launcher_is_killed_and_reported() {
        let command = launcher_command("powershell.exe", "Start-Sleep -Seconds 120");
        let started = std::time::Instant::now();
        assert_eq!(
            run_launcher(command, "https://auth.axiusflow.com/sign-in?x=1&y=2"),
            Err("system browser could not be opened".to_string())
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "hung launcher must be killed on the timeout, not waited out"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{MAXIMUM_AUTHORIZATION_URL_BYTES, open_system_browser};

    #[test]
    fn non_https_and_malformed_urls_fail_closed_without_spawning() {
        for url in [
            "",
            "http://auth.axiusflow.com/authorize",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://auth.axiusflow.com/has space",
            "127.0.0.1:8080/callback",
        ] {
            assert!(
                open_system_browser(url).is_err(),
                "unsafe URL must not open: {url}"
            );
        }
    }

    #[test]
    fn overlong_urls_fail_closed() {
        let long = format!(
            "https://auth.axiusflow.com/{}",
            "a".repeat(MAXIMUM_AUTHORIZATION_URL_BYTES)
        );
        assert!(open_system_browser(&long).is_err());
    }
}
