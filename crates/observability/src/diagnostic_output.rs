//! Bounded process diagnostics, rotating logs, and durable panic reports.

use std::{
    backtrace::Backtrace,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    panic,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, SecondsFormat, Utc};

pub const LOG_CHANNEL_CAPACITY: usize = 1_024;
pub const DEFAULT_LOG_FILE_BYTES: u64 = 4 * 1024 * 1024;
const ROTATED_LOG_FILES: usize = 4;
const GUARD_TIMEOUT: Duration = Duration::from_secs(2);
const MAXIMUM_CRASH_REPORTS: usize = 10;

static ACTIVE_SINK: OnceLock<Mutex<Option<ActiveSink>>> = OnceLock::new();
static NEXT_SINK_ID: AtomicU64 = AtomicU64::new(1);

struct ActiveSink {
    id: u64,
    sender: SyncSender<Command>,
    dropped: Arc<AtomicU64>,
}

enum Command {
    Line { text: String, dropped_before: u64 },
    Flush(SyncSender<()>),
    Shutdown(SyncSender<()>),
}

/// Owns the diagnostics writer and gives shutdown at most two seconds to flush and join.
pub struct LogSinkGuard {
    id: u64,
    sender: Option<SyncSender<Command>>,
    worker: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl LogSinkGuard {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LogSinkGuard {
    fn drop(&mut self) {
        let Some(sender) = self.sender.take() else {
            return;
        };
        if let Ok(mut active) = active_sink().lock()
            && active.as_ref().is_some_and(|sink| sink.id == self.id)
        {
            active.take();
        }

        let deadline = Instant::now() + GUARD_TIMEOUT;
        let (done_sender, done_receiver) = mpsc::sync_channel(1);
        let mut command = Command::Shutdown(done_sender);
        loop {
            match sender.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    command = returned;
                    thread::yield_now();
                }
                Err(_) => return,
            }
        }
        drop(sender);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if done_receiver.recv_timeout(remaining).is_ok()
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}

/// Installs one process-wide, nonblocking rotating log sink.
///
/// The current file is limited to 4 MiB and `aeris.1.log` through
/// `aeris.4.log` retain the four previous files. Standard error remains active.
///
/// # Errors
/// Returns an error if the directory/file cannot be opened, the writer cannot
/// be started, or a sink is already installed.
pub fn install_log_sink(path: impl AsRef<Path>) -> io::Result<LogSinkGuard> {
    start_log_sink(path.as_ref(), DEFAULT_LOG_FILE_BYTES, true)
}

/// Gives the active diagnostics writer a bounded opportunity to durably flush
/// every line accepted before this call.
#[must_use]
pub fn flush_diagnostics() -> bool {
    let sender = {
        let Ok(active) = active_sink().lock() else {
            return false;
        };
        let Some(sink) = active.as_ref() else {
            return true;
        };
        sink.sender.clone()
    };
    let deadline = Instant::now() + GUARD_TIMEOUT;
    let (done_sender, done_receiver) = mpsc::sync_channel(1);
    let mut command = Command::Flush(done_sender);
    loop {
        match sender.try_send(command) {
            Ok(()) => break,
            Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                command = returned;
                thread::yield_now();
            }
            Err(_) => return false,
        }
    }
    done_receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .is_ok()
}

fn start_log_sink(path: &Path, maximum_bytes: u64, register: bool) -> io::Result<LogSinkGuard> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file = open_log(path)?;
    let (sender, receiver) = mpsc::sync_channel(LOG_CHANNEL_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let worker_path = path.to_path_buf();
    let worker = thread::Builder::new()
        .name("aeris-log-writer".to_string())
        .spawn(move || run_writer(&receiver, file, &worker_path, maximum_bytes))?;
    let id = NEXT_SINK_ID.fetch_add(1, Ordering::Relaxed);
    if register {
        let mut active = active_sink()
            .lock()
            .map_err(|_| io::Error::other("diagnostic sink lock is unavailable"))?;
        if active.is_some() {
            drop(sender);
            let _ = worker.join();
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "diagnostic sink is already installed",
            ));
        }
        *active = Some(ActiveSink {
            id,
            sender: sender.clone(),
            dropped,
        });
    }
    Ok(LogSinkGuard {
        id,
        sender: Some(sender),
        worker: Some(worker),
        path: path.to_path_buf(),
    })
}

fn active_sink() -> &'static Mutex<Option<ActiveSink>> {
    ACTIVE_SINK.get_or_init(|| Mutex::new(None))
}

/// Writes one line to standard error and offers it to the bounded file sink.
#[macro_export]
macro_rules! diagnostic {
    ($($argument:tt)+) => {
        $crate::write_diagnostic(::std::format_args!($($argument)+))
    };
}

#[doc(hidden)]
pub fn write_diagnostic(arguments: fmt::Arguments<'_>) {
    let message = arguments.to_string();
    write_stderr_line(&message);
    let Ok(active) = active_sink().lock() else {
        return;
    };
    let Some(sink) = active.as_ref() else {
        return;
    };
    try_send_line(&sink.sender, &sink.dropped, formatted_line(&message));
}

fn try_send_line(sender: &SyncSender<Command>, dropped: &AtomicU64, text: String) {
    let dropped_before = dropped.swap(0, Ordering::AcqRel);
    match sender.try_send(Command::Line {
        text,
        dropped_before,
    }) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(dropped_before.saturating_add(1), Ordering::Relaxed);
        }
    }
}

fn write_stderr_line(message: &str) {
    let _ = writeln!(io::stderr().lock(), "{message}");
}

fn formatted_line(message: &str) -> String {
    let timestamp: DateTime<Utc> = SystemTime::now().into();
    let current = thread::current();
    let thread_name = current.name().unwrap_or("unnamed");
    format!(
        "{} [{thread_name}] {message}\n",
        timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
    )
}

fn open_log(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn run_writer(receiver: &Receiver<Command>, mut file: File, path: &Path, maximum_bytes: u64) {
    let mut size = file.metadata().map_or(0, |metadata| metadata.len());
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Line {
                text,
                dropped_before,
            } => {
                if dropped_before > 0 {
                    let notice =
                        formatted_line(&format!("diagnostics dropped {dropped_before} lines"));
                    write_rotating(&mut file, path, maximum_bytes, &mut size, &notice);
                }
                write_rotating(&mut file, path, maximum_bytes, &mut size, &text);
            }
            Command::Flush(done) => {
                let _ = file.flush();
                let _ = file.sync_all();
                let _ = done.send(());
            }
            Command::Shutdown(done) => {
                let _ = file.flush();
                let _ = file.sync_all();
                let _ = done.send(());
                return;
            }
        }
    }
    let _ = file.flush();
}

fn write_rotating(file: &mut File, path: &Path, maximum_bytes: u64, size: &mut u64, line: &str) {
    let line_bytes = u64::try_from(line.len()).unwrap_or(u64::MAX);
    if *size > 0
        && size.saturating_add(line_bytes) > maximum_bytes
        && rotate(path).is_ok()
        && let Ok(reopened) = open_log(path)
    {
        *file = reopened;
        *size = 0;
    }
    if file.write_all(line.as_bytes()).is_ok() {
        *size = size.saturating_add(line_bytes);
    }
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let file_name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let rotated_name = match file_name.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}.{index}.{extension}"),
        None => format!("{file_name}.{index}"),
    };
    path.with_file_name(rotated_name)
}

fn rotate(path: &Path) -> io::Result<()> {
    let oldest = rotated_path(path, ROTATED_LOG_FILES);
    match fs::remove_file(oldest) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for index in (1..ROTATED_LOG_FILES).rev() {
        let source = rotated_path(path, index);
        let destination = rotated_path(path, index + 1);
        match aeris_platform_sys::fs::move_file(&source, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    aeris_platform_sys::fs::move_file(path, &rotated_path(path, 1))
}

/// Installs a panic hook that durably records a bounded crash report and then
/// invokes the previously installed hook.
///
/// # Errors
/// Returns an error when the crash-report directory cannot be created.
pub fn install_panic_hook(
    directory: impl AsRef<Path>,
    release_identity: impl Into<String>,
    install_generation: u64,
) -> io::Result<()> {
    let directory = directory.as_ref().to_path_buf();
    fs::create_dir_all(&directory)?;
    let release_identity = release_identity.into();
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let timestamp = unix_milliseconds();
        let thread = thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");
        let location = info
            .location()
            .map_or_else(|| "unknown".to_string(), ToString::to_string);
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        let report = format!(
            "timestamp_unix_ms={timestamp}\nrelease_identity={release_identity}\n\
             install_generation={install_generation}\nthread={thread_name}\nlocation={location}\n\
             message={message}\nbacktrace={}\n",
            Backtrace::force_capture()
        );
        let path = directory.join(format!("crash-{timestamp}.log"));
        let _ = aeris_platform_sys::fs::write_file_durably(&path, report.as_bytes());
        retain_newest_crashes(&directory);
        previous(info);
    }));
    Ok(())
}

fn unix_milliseconds() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn retain_newest_crashes(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut crashes = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("crash-") && name.ends_with(".log")
        })
        .collect::<Vec<_>>();
    crashes.sort_by_key(std::fs::DirEntry::file_name);
    let remove_count = crashes.len().saturating_sub(MAXIMUM_CRASH_REPORTS);
    for entry in crashes.into_iter().take(remove_count) {
        let _ = fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as ProcessCommand;

    const PANIC_CHILD: &str = "AERIS_OBSERVABILITY_PANIC_CHILD";
    const FLUSH_EXIT_CHILD: &str = "AERIS_OBSERVABILITY_FLUSH_EXIT_CHILD";

    fn fixture(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aeris-observability-{label}-{}-{}",
            std::process::id(),
            unix_milliseconds()
        ))
    }

    #[test]
    fn rotation_retains_current_and_four_previous_files() {
        let root = fixture("rotation");
        fs::create_dir_all(&root).expect("fixture");
        let path = root.join("aeris.log");
        let guard = start_log_sink(&path, 80, false).expect("sink");
        let sender = guard.sender.as_ref().expect("sender");
        for index in 0..16 {
            sender
                .send(Command::Line {
                    text: format!("line-{index:02}-xxxxxxxxxxxxxxxx\n"),
                    dropped_before: 0,
                })
                .expect("line");
        }
        drop(guard);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("aeris.log")
        );
        assert!(path.exists());
        for index in 1..=4 {
            let rotated = root.join(format!("aeris.{index}.log"));
            assert_eq!(rotated_path(&path, index), rotated);
            assert!(rotated.exists());
        }
        assert!(!root.join("aeris.5.log").exists());
        assert!(!root.join("aeris.log.1").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn dropped_count_precedes_the_next_line() {
        let root = fixture("drop");
        fs::create_dir_all(&root).expect("fixture");
        let path = root.join("aeris.log");
        let (sender, receiver) = mpsc::sync_channel(4);
        let file = open_log(&path).expect("file");
        sender
            .send(Command::Line {
                text: "next-line\n".to_string(),
                dropped_before: 7,
            })
            .expect("line");
        let (done_sender, done_receiver) = mpsc::sync_channel(1);
        sender
            .send(Command::Shutdown(done_sender))
            .expect("shutdown");
        run_writer(&receiver, file, &path, 1024);
        done_receiver.recv().expect("done");
        let contents = fs::read_to_string(&path).expect("contents");
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("diagnostics dropped 7 lines"));
        assert!(!lines[0].contains("dropped before"));
        assert_eq!(lines[1], "next-line");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn guard_flushes_and_joins_with_queued_lines() {
        let root = fixture("guard");
        fs::create_dir_all(&root).expect("fixture");
        let path = root.join("aeris.log");
        let guard = start_log_sink(&path, 1024, false).expect("sink");
        guard
            .sender
            .as_ref()
            .expect("sender")
            .send(Command::Line {
                text: "queued\n".to_string(),
                dropped_before: 0,
            })
            .expect("line");
        let started = Instant::now();
        drop(guard);
        assert!(started.elapsed() <= GUARD_TIMEOUT);
        assert_eq!(fs::read_to_string(&path).expect("contents"), "queued\n");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn overflow_is_nonblocking_and_attaches_count_to_next_line() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let dropped = AtomicU64::new(0);
        sender
            .send(Command::Line {
                text: "occupied".to_string(),
                dropped_before: 0,
            })
            .expect("occupied");
        let started = Instant::now();
        try_send_line(&sender, &dropped, "dropped".to_string());
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let _ = receiver.recv().expect("occupied line");
        try_send_line(&sender, &dropped, "next".to_string());
        let Command::Line {
            text,
            dropped_before,
        } = receiver.recv().expect("next line")
        else {
            panic!("expected a line");
        };
        assert_eq!(text, "next");
        assert_eq!(dropped_before, 1);
    }

    #[test]
    fn formatted_lines_include_rfc3339_utc_and_thread_name_without_secret_material() {
        const SECRET_SENTINEL: &str = "AERIS_TEST_SECRET_MUST_NOT_APPEAR";
        let line = formatted_line("safe diagnostic");
        let (timestamp, remainder) = line.split_once(" [").expect("timestamp separator");
        assert!(DateTime::parse_from_rfc3339(timestamp).is_ok());
        assert!(timestamp.ends_with('Z'));
        assert!(remainder.contains("] safe diagnostic\n"));
        assert!(!line.contains(SECRET_SENTINEL));
    }

    #[test]
    fn panic_child_writes_durable_bounded_report() {
        if let Some(root) = std::env::var_os(PANIC_CHILD) {
            panic::set_hook(Box::new(|info| {
                eprintln!("previous panic hook observed: {info}");
            }));
            install_panic_hook(&root, "test-release", 42).expect("panic hook");
            panic!("forced diagnostics panic");
        }
        let root = fixture("panic");
        fs::create_dir_all(&root).expect("fixture");
        for index in 0..12 {
            aeris_platform_sys::fs::write_file_durably(
                &root.join(format!("crash-{index:013}.log")),
                b"old",
            )
            .expect("old crash");
        }
        let old_names = fs::read_dir(&root)
            .expect("old reports")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<std::collections::BTreeSet<_>>();
        let output = ProcessCommand::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "diagnostic_output::tests::panic_child_writes_durable_bounded_report",
                "--nocapture",
            ])
            .env(PANIC_CHILD, &root)
            .output()
            .expect("panic child");
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).expect("panic stderr");
        assert!(stderr.contains("previous panic hook observed:"));
        assert!(stderr.contains("forced diagnostics panic"));
        let mut reports = fs::read_dir(&root)
            .expect("reports")
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        reports.sort_by_key(std::fs::DirEntry::file_name);
        assert_eq!(reports.len(), MAXIMUM_CRASH_REPORTS);
        let new_reports = reports
            .iter()
            .filter(|entry| !old_names.contains(&entry.file_name()))
            .collect::<Vec<_>>();
        assert_eq!(new_reports.len(), 1);
        let report = fs::read_to_string(new_reports[0].path()).expect("report contents");
        assert!(report.contains("message=forced diagnostics panic"));
        assert!(report.contains("release_identity=test-release"));
        assert!(report.contains("install_generation=42"));
        assert!(report.contains(
            "thread=diagnostic_output::tests::panic_child_writes_durable_bounded_report"
        ));
        assert!(report.contains("location=crates\\observability\\src\\diagnostic_output.rs:"));
        let backtrace = report.split_once("backtrace=").expect("backtrace field").1;
        assert_ne!(backtrace.trim(), "");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn explicit_flush_persists_the_final_line_before_process_exit() {
        if let Some(root) = std::env::var_os(FLUSH_EXIT_CHILD) {
            let _guard = install_log_sink(PathBuf::from(root).join("aeris.log")).expect("sink");
            crate::diagnostic!("final child diagnostic");
            assert!(flush_diagnostics());
            std::process::exit(0);
        }
        let root = fixture("flush-exit");
        fs::create_dir_all(&root).expect("fixture");
        let status = ProcessCommand::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "diagnostic_output::tests::explicit_flush_persists_the_final_line_before_process_exit",
                "--nocapture",
            ])
            .env(FLUSH_EXIT_CHILD, &root)
            .status()
            .expect("flush child");
        assert!(status.success());
        let contents = fs::read_to_string(root.join("aeris.log")).expect("contents");
        assert!(
            contents
                .lines()
                .last()
                .is_some_and(|line| line.ends_with("] final child diagnostic"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
