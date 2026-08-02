//! Registered URI-scheme authorization callback.
//!
//! Where the operating system can route a private scheme back to this application, the
//! authorization redirect arrives as a URI argument on a fresh process launch instead of
//! an HTTP request. This adapter registers that scheme and verifies a delivered redirect
//! under the same rules the loopback listener applies: bounded input, constant-time
//! `state` comparison before any error path, and a sanitized server error.

use crate::CapabilityAvailability;
use crate::loopback_callback::{
    AuthorizationCode, constant_time_equals, percent_decode, sanitize_server_error,
};
use crate::pkce::PkceSecret;
use core::fmt;
use std::error::Error;
use std::path::{Path, PathBuf};

/// Largest redirect URI accepted from the operating system.
pub const MAXIMUM_REDIRECT_URI_BYTES: usize = 2_048;
/// Bytes reserved within [`MAXIMUM_REDIRECT_URI_BYTES`] for the authorization response.
pub const MAXIMUM_REDIRECT_RESPONSE_BYTES: usize = 512;
/// Largest registered redirect base, leaving room for the delivered response query.
pub const MAXIMUM_REDIRECT_BASE_BYTES: usize =
    MAXIMUM_REDIRECT_URI_BYTES - MAXIMUM_REDIRECT_RESPONSE_BYTES;
/// Largest private scheme accepted for registration.
pub const MAXIMUM_URI_SCHEME_BYTES: usize = 64;
/// Largest application identifier accepted for a desktop registration entry.
pub const MAXIMUM_APPLICATION_ID_BYTES: usize = 128;

#[cfg(target_os = "linux")]
const DESKTOP_ENTRY_DIRECTORY: &str = "applications";
/// XDG file that records which desktop entry handles each MIME type.
#[cfg(target_os = "linux")]
const MIME_ASSOCIATION_FILE: &str = "mimeapps.list";
/// Lock that serializes a whole registration transaction across processes.
#[cfg(target_os = "linux")]
const REGISTRATION_LOCK_FILE: &str = ".axiusflow-uri-registration-lock";
/// Sibling lock that serializes the association read-modify-write across processes.
#[cfg(target_os = "linux")]
const MIME_ASSOCIATION_LOCK_FILE: &str = "mimeapps.list.axiusflow-lock";
/// Section of [`MIME_ASSOCIATION_FILE`] that names default handlers.
#[cfg(target_os = "linux")]
const DEFAULT_APPLICATIONS_SECTION: &str = "[Default Applications]";
/// Largest existing association file this adapter will rewrite.
#[cfg(target_os = "linux")]
const MAXIMUM_MIME_ASSOCIATION_BYTES: u64 = 256 * 1_024;

/// Reason a registered URI-scheme callback could not be registered or verified.
#[derive(Debug)]
pub enum UriCallbackError {
    UnsupportedPlatform,
    InvalidScheme,
    ReservedScheme,
    InvalidApplicationId,
    InvalidRedirectUri,
    RedirectUriTooLong,
    RedirectTargetMismatch,
    MissingAuthorizationCode,
    MissingState,
    StateMismatch,
    AuthorizationServerError(String),
    InvalidExecutablePath,
    DesktopDatabaseRefreshUnavailable,
    DesktopDatabaseRefreshFailed,
    InvalidMimeAssociations,
    Io(std::io::Error),
}

impl fmt::Display for UriCallbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                formatter.write_str("no registered URI-scheme adapter exists for this platform")
            }
            Self::InvalidScheme => formatter.write_str("registered URI scheme is invalid"),
            Self::ReservedScheme => {
                formatter.write_str("registered URI scheme may not claim a reserved web scheme")
            }
            Self::InvalidApplicationId => {
                formatter.write_str("registered application identifier is invalid")
            }
            Self::InvalidRedirectUri => formatter.write_str("registered redirect URI is invalid"),
            Self::RedirectUriTooLong => {
                formatter.write_str("delivered redirect URI exceeded its bound")
            }
            Self::RedirectTargetMismatch => {
                formatter.write_str("delivered redirect targeted another URI")
            }
            Self::MissingAuthorizationCode => {
                formatter.write_str("delivered redirect omitted the authorization code")
            }
            Self::MissingState => formatter.write_str("delivered redirect omitted the CSRF state"),
            Self::StateMismatch => {
                formatter.write_str("delivered redirect state did not match the generated state")
            }
            Self::AuthorizationServerError(error) => {
                write!(formatter, "authorization server reported: {error}")
            }
            Self::InvalidExecutablePath => formatter.write_str(
                "registered executable path cannot be expressed as a desktop-entry command",
            ),
            Self::DesktopDatabaseRefreshUnavailable => formatter.write_str(
                "no desktop MIME database tool is available to activate the scheme handler",
            ),
            Self::DesktopDatabaseRefreshFailed => {
                formatter.write_str("desktop MIME database refresh rejected the scheme handler")
            }
            Self::InvalidMimeAssociations => {
                formatter.write_str("existing desktop MIME associations could not be rewritten")
            }
            Self::Io(error) => write!(formatter, "URI-scheme registration failed: {error}"),
        }
    }
}

impl Error for UriCallbackError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// A private redirect URI this application owns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredUriRedirect {
    scheme: String,
    redirect_uri: String,
}

impl RegisteredUriRedirect {
    /// Validates a private redirect URI such as `com.example.app:/authorized`.
    ///
    /// The base is bounded below [`MAXIMUM_REDIRECT_URI_BYTES`] by
    /// [`MAXIMUM_REDIRECT_RESPONSE_BYTES`], because every delivered redirect appends a
    /// query that must still fit; otherwise a base could be accepted that can never
    /// produce a verifiable authorization code.
    ///
    /// # Errors
    ///
    /// Returns an error when the scheme is malformed, claims a reserved web scheme, the URI
    /// already carries a query or fragment that a redirect would have to replace, the
    /// remainder is not valid RFC 3986 path syntax, or no response room remains.
    pub fn try_new(redirect_uri: impl Into<String>) -> Result<Self, UriCallbackError> {
        let redirect_uri = redirect_uri.into();
        if redirect_uri.is_empty() || redirect_uri.len() > MAXIMUM_REDIRECT_BASE_BYTES {
            return Err(UriCallbackError::InvalidRedirectUri);
        }
        if redirect_uri.contains('?') || redirect_uri.contains('#') {
            return Err(UriCallbackError::InvalidRedirectUri);
        }
        let (scheme, remainder) = redirect_uri
            .split_once(':')
            .ok_or(UriCallbackError::InvalidRedirectUri)?;
        validate_scheme(scheme)?;
        validate_uri_remainder(remainder)?;
        Ok(Self {
            scheme: scheme.to_owned(),
            redirect_uri,
        })
    }

    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Verifies a redirect URI delivered by the operating system.
    ///
    /// Per RFC 6749 section 4.1.2.1 the echoed `state` is verified in constant time before
    /// any `error` response is honored, so an unverified caller can neither abort the
    /// pending sign-in nor inject text into diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized URI, a foreign redirect target, a missing or
    /// forged `state`, a state-verified server error, or a missing authorization code.
    pub fn verify_redirect(
        &self,
        delivered_uri: &str,
        secret: &PkceSecret,
    ) -> Result<AuthorizationCode, UriCallbackError> {
        if delivered_uri.len() > MAXIMUM_REDIRECT_URI_BYTES {
            return Err(UriCallbackError::RedirectUriTooLong);
        }
        let (target, query) = delivered_uri
            .split_once('?')
            .map_or((delivered_uri, ""), |(target, query)| (target, query));
        let query = query.split_once('#').map_or(query, |(query, _)| query);
        if target != self.redirect_uri {
            return Err(UriCallbackError::RedirectTargetMismatch);
        }

        let mut code = None;
        let mut state = None;
        let mut server_error = None;
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name {
                "code" => code = Some(percent_decode(value)),
                "state" => state = Some(percent_decode(value)),
                "error" => server_error = Some(percent_decode(value)),
                _ => {}
            }
        }

        let state = state.ok_or(UriCallbackError::MissingState)?;
        if !constant_time_equals(state.as_bytes(), secret.state().as_bytes()) {
            return Err(UriCallbackError::StateMismatch);
        }
        if let Some(error) = server_error {
            return Err(UriCallbackError::AuthorizationServerError(
                sanitize_server_error(&error),
            ));
        }
        let code = code.ok_or(UriCallbackError::MissingAuthorizationCode)?;
        if code.is_empty() {
            return Err(UriCallbackError::MissingAuthorizationCode);
        }
        Ok(AuthorizationCode::new(code))
    }
}

/// Registers a private URI scheme with the operating system.
#[derive(Debug)]
pub struct NativeUriSchemeRegistrar;

impl NativeUriSchemeRegistrar {
    /// Reports whether this crate implements scheme registration for the running target.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(target_os = "linux")]
        return CapabilityAvailability::Available;

        #[cfg(not(target_os = "linux"))]
        CapabilityAvailability::Unavailable
    }

    /// Writes the XDG scheme-handler entry without refreshing the desktop MIME database.
    ///
    /// Callers that manage MIME activation themselves use this; everything else should use
    /// [`Self::register_in`], which also activates the handler. Returns the entry path.
    ///
    /// # Errors
    ///
    /// Returns an error on an unsupported target, an invalid identifier or executable path,
    /// or durable I/O failure.
    #[cfg(target_os = "linux")]
    pub fn write_entry_in(
        data_home: impl AsRef<Path>,
        application_id: &str,
        executable: impl AsRef<Path>,
        redirect: &RegisteredUriRedirect,
    ) -> Result<PathBuf, UriCallbackError> {
        Self::write_entry(
            data_home.as_ref(),
            application_id,
            executable.as_ref(),
            redirect,
        )
    }

    /// Registers `redirect` as the default XDG handler for its scheme.
    ///
    /// `data_home` and `config_home` must already exist so this adapter never has to prove
    /// that a directory it created reached durable storage. The desktop entry is written to
    /// a uniquely named temporary file, synced, and renamed into place, and every directory
    /// whose entries changed is synced, so a crash cannot leave a partially written or
    /// unreferenced handler. Because rebuilding the capability cache alone does not route a
    /// scheme, this also records the entry under `[Default Applications]` in
    /// `mimeapps.list`, preserving every other association, before reporting success.
    ///
    /// A failing activation step never leaves a previously working handler destroyed: an
    /// existing entry for `application_id` is preserved and restored if the MIME refresh or
    /// association update fails, and an entry this call created is removed instead.
    ///
    /// # Errors
    ///
    /// Returns an error on an unsupported target, an invalid identifier or executable path,
    /// an unavailable or failing desktop MIME database tool, unreadable existing
    /// associations, or durable I/O failure.
    #[cfg(target_os = "linux")]
    pub fn register_in(
        data_home: impl AsRef<Path>,
        config_home: impl AsRef<Path>,
        application_id: &str,
        executable: impl AsRef<Path>,
        redirect: &RegisteredUriRedirect,
    ) -> Result<PathBuf, UriCallbackError> {
        use std::fs;

        let data_home = data_home.as_ref();
        let config_home = config_home.as_ref();
        let directory = data_home.join(DESKTOP_ENTRY_DIRECTORY);
        let final_path = directory.join(format!("{application_id}.desktop"));
        // Snapshot, replacement, activation, and rollback form one transaction. Without a
        // lock held across all of it, two concurrent registrations could snapshot the same
        // prior entry and a failing one could then undo a successful one.
        let _lock = acquire_lock(&data_home.join(REGISTRATION_LOCK_FILE))?;
        let previous_entry = read_optional_file(&final_path)?;
        let previous_association = read_optional_file(&config_home.join(MIME_ASSOCIATION_FILE))?;

        let entry = Self::write_entry(data_home, application_id, executable.as_ref(), redirect)?;
        let activated = refresh_desktop_database(&directory).and_then(|()| {
            set_default_handler(
                config_home,
                redirect.scheme(),
                &format!("{application_id}.desktop"),
            )
        });
        if let Err(error) = activated {
            if let Some(contents) = previous_entry {
                write_file_atomically(&directory, &final_path, &contents)?;
            } else {
                fs::remove_file(&final_path).map_err(UriCallbackError::Io)?;
                sync_directory(&directory)?;
            }
            // The association may already have been renamed into place before a later step
            // failed, so put the previous associations back rather than leaving a committed
            // default that points at an entry this call just removed or restored.
            restore_association(config_home, previous_association)?;
            // The capability cache still advertises the replaced or deleted entry, so
            // refresh it against the restored state. The activation error stays primary.
            let _ = refresh_desktop_database(&directory);
            return Err(error);
        }
        Ok(entry)
    }

    #[cfg(target_os = "linux")]
    fn write_entry(
        data_home: &Path,
        application_id: &str,
        executable: &Path,
        redirect: &RegisteredUriRedirect,
    ) -> Result<PathBuf, UriCallbackError> {
        use std::fs;
        use std::io::Write;

        validate_application_id(application_id)?;
        let command = desktop_exec_command(executable)?;

        let directory = data_home.join(DESKTOP_ENTRY_DIRECTORY);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(UriCallbackError::InvalidApplicationId),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A concurrent registration may create the directory first; that is the
                // same successful end state this call was trying to reach.
                match fs::create_dir(&directory) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if !fs::symlink_metadata(&directory)
                            .map_err(UriCallbackError::Io)?
                            .is_dir()
                        {
                            return Err(UriCallbackError::InvalidApplicationId);
                        }
                    }
                    Err(error) => return Err(UriCallbackError::Io(error)),
                }
                // Persist the new directory's entry in its already-durable parent before
                // anything relies on that directory existing.
                sync_directory(data_home)?;
            }
            Err(error) => return Err(UriCallbackError::Io(error)),
        }

        let entry = format!(
            "[Desktop Entry]\nType=Application\nName={application_id}\nExec={command} %u\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/{scheme};\n",
            scheme = redirect.scheme()
        );
        let final_path = directory.join(format!("{application_id}.desktop"));
        // A collision-resistant name keeps concurrent registrations, and any leftover from
        // an earlier crash, from unlinking or renaming each other's in-flight file.
        let (temporary_path, mut file) =
            create_temporary_sibling(&directory, &format!("{application_id}.desktop"))?;
        let written = file
            .write_all(entry.as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(error) = written {
            drop(file);
            let _ = fs::remove_file(&temporary_path);
            return Err(UriCallbackError::Io(error));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary_path, &final_path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(UriCallbackError::Io(error));
        }
        sync_directory(&directory)?;
        Ok(final_path)
    }

    /// Entry writing is unavailable on targets without an implemented adapter.
    ///
    /// # Errors
    ///
    /// Always returns [`UriCallbackError::UnsupportedPlatform`].
    #[cfg(not(target_os = "linux"))]
    pub fn write_entry_in(
        _data_home: impl AsRef<Path>,
        _application_id: &str,
        _executable: impl AsRef<Path>,
        _redirect: &RegisteredUriRedirect,
    ) -> Result<PathBuf, UriCallbackError> {
        Err(UriCallbackError::UnsupportedPlatform)
    }

    /// Registration is unavailable on targets without an implemented adapter.
    ///
    /// # Errors
    ///
    /// Always returns [`UriCallbackError::UnsupportedPlatform`].
    #[cfg(not(target_os = "linux"))]
    pub fn register_in(
        _data_home: impl AsRef<Path>,
        _config_home: impl AsRef<Path>,
        _application_id: &str,
        _executable: impl AsRef<Path>,
        _redirect: &RegisteredUriRedirect,
    ) -> Result<PathBuf, UriCallbackError> {
        Err(UriCallbackError::UnsupportedPlatform)
    }
}

/// Serializes an executable path as a single Desktop Entry `Exec` argument.
///
/// The Desktop Entry specification splits `Exec` on whitespace and expands `%` field
/// codes, so an unquoted path containing a space, backslash, or `%` would produce a
/// handler that cannot launch the intended binary.
#[cfg(target_os = "linux")]
fn desktop_exec_command(executable: &Path) -> Result<String, UriCallbackError> {
    // Desktop Entry string values prohibit control characters outright, so a path holding
    // one would install a handler that conforming launchers refuse.
    let path = executable
        .to_str()
        .filter(|path| !path.is_empty() && !path.chars().any(char::is_control))
        .ok_or(UriCallbackError::InvalidExecutablePath)?;
    let mut command = String::with_capacity(path.len() + 2);
    command.push('"');
    for character in path.chars() {
        match character {
            // Desktop Entry applies generic string escaping before command-line parsing,
            // so one literal backslash must survive both layers as four backslashes.
            '\\' => command.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                command.push_str("\\\\");
                command.push(character);
            }
            '%' => command.push_str("%%"),
            _ => command.push(character),
        }
    }
    command.push('"');
    Ok(command)
}

/// Reads a file that may be absent, distinguishing absence from a real I/O failure.
#[cfg(target_os = "linux")]
fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, UriCallbackError> {
    match std::fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(UriCallbackError::Io(error)),
    }
}

/// Takes an exclusive advisory lock, creating the lock file when absent.
#[cfg(target_os = "linux")]
fn acquire_lock(path: &Path) -> Result<std::fs::File, UriCallbackError> {
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(UriCallbackError::Io)?;
    lock.lock().map_err(UriCallbackError::Io)?;
    Ok(lock)
}

/// Restores the previous MIME associations, or removes a file this call introduced.
#[cfg(target_os = "linux")]
fn restore_association(
    config_home: &Path,
    previous: Option<Vec<u8>>,
) -> Result<(), UriCallbackError> {
    let association_path = config_home.join(MIME_ASSOCIATION_FILE);
    if let Some(contents) = previous {
        return write_file_atomically(config_home, &association_path, &contents);
    }
    match std::fs::remove_file(&association_path) {
        Ok(()) => sync_directory(config_home),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(UriCallbackError::Io(error)),
    }
}

/// Replaces a file's contents through a synced temporary file and rename.
#[cfg(target_os = "linux")]
fn write_file_atomically(
    directory: &Path,
    final_path: &Path,
    contents: &[u8],
) -> Result<(), UriCallbackError> {
    use std::fs;
    use std::io::Write;

    let (temporary_path, mut file) = create_temporary_sibling(directory, "replace")?;
    let written = file.write_all(contents).and_then(|()| file.sync_all());
    if let Err(error) = written {
        drop(file);
        let _ = fs::remove_file(&temporary_path);
        return Err(UriCallbackError::Io(error));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary_path, final_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(UriCallbackError::Io(error));
    }
    sync_directory(directory)
}

/// Builds a collision-resistant temporary sibling path for an atomic replacement.
///
/// A crash leaves the previous `.partial` behind, and Linux reuses process identifiers, so
/// a name derived only from the identifier and a fresh counter can collide with that
/// leftover and fail `create_new`. Mixing in a monotonic nanosecond timestamp and retrying
/// keeps a stale file from blocking a later registration.
#[cfg(target_os = "linux")]
fn create_temporary_sibling(
    directory: &Path,
    stem: &str,
) -> Result<(PathBuf, std::fs::File), UriCallbackError> {
    use std::fs::OpenOptions;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

    let mut last_error = None;
    for _ in 0..16 {
        let unique = format!(
            "{}.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
            NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        );
        let candidate = directory.join(format!("{stem}.{unique}.partial"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => return Err(UriCallbackError::Io(error)),
        }
    }
    Err(UriCallbackError::Io(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no unique temporary path was available",
        )
    })))
}

/// Records `entry` as the default handler for `scheme` in `mimeapps.list`.
///
/// Rebuilding the capability cache only advertises that an entry *can* handle a scheme.
/// Routing requires the default association, so this writes it while preserving every
/// other line, then syncs the replacement into place.
#[cfg(target_os = "linux")]
fn set_default_handler(
    config_home: &Path,
    scheme: &str,
    entry: &str,
) -> Result<(), UriCallbackError> {
    use std::fs;
    use std::io::Write;

    match fs::symlink_metadata(config_home) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) | Err(_) => return Err(UriCallbackError::InvalidMimeAssociations),
    }
    let association_path = config_home.join(MIME_ASSOCIATION_FILE);
    // Unique temporary paths stop two registrations from clobbering one file, but not the
    // lost-update race where both read the same associations and each replaces the whole
    // file. An exclusive lock on a sibling makes the read-modify-write atomic across
    // processes without locking the file that gets renamed away.
    let _lock = acquire_lock(&config_home.join(MIME_ASSOCIATION_LOCK_FILE))?;
    let existing = match fs::symlink_metadata(&association_path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > MAXIMUM_MIME_ASSOCIATION_BYTES {
                return Err(UriCallbackError::InvalidMimeAssociations);
            }
            fs::read_to_string(&association_path).map_err(UriCallbackError::Io)?
        }
        Ok(_) => return Err(UriCallbackError::InvalidMimeAssociations),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(UriCallbackError::Io(error)),
    };

    let association = format!("x-scheme-handler/{scheme}");
    let default_line = format!("{association}={entry}\n");
    let mut rendered = String::with_capacity(existing.len() + association.len() + entry.len() + 64);
    let mut in_defaults = false;
    let mut wrote_association = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_defaults && !wrote_association {
                rendered.push_str(&default_line);
                wrote_association = true;
            }
            in_defaults = trimmed == DEFAULT_APPLICATIONS_SECTION;
            rendered.push_str(line);
            rendered.push('\n');
            continue;
        }
        // Replace only this scheme's default, leaving every other association intact.
        if in_defaults
            && trimmed
                .split_once('=')
                .is_some_and(|(name, _)| name.trim() == association)
        {
            if !wrote_association {
                rendered.push_str(&default_line);
                wrote_association = true;
            }
            continue;
        }
        rendered.push_str(line);
        rendered.push('\n');
    }
    if !wrote_association {
        if !in_defaults {
            rendered.push_str(DEFAULT_APPLICATIONS_SECTION);
            rendered.push('\n');
        }
        rendered.push_str(&default_line);
    }

    let (temporary_path, mut file) = create_temporary_sibling(config_home, MIME_ASSOCIATION_FILE)?;
    let written = file
        .write_all(rendered.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        drop(file);
        let _ = fs::remove_file(&temporary_path);
        return Err(UriCallbackError::Io(error));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary_path, &association_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(UriCallbackError::Io(error));
    }
    sync_directory(config_home)
}

/// Refreshes the desktop MIME cache so the scheme handler actually resolves.
#[cfg(target_os = "linux")]
fn refresh_desktop_database(directory: &Path) -> Result<(), UriCallbackError> {
    use std::process::{Command, Stdio};

    let status = Command::new("update-desktop-database")
        .arg(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(UriCallbackError::DesktopDatabaseRefreshFailed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(UriCallbackError::DesktopDatabaseRefreshUnavailable)
        }
        Err(error) => Err(UriCallbackError::Io(error)),
    }
}

#[cfg(target_os = "linux")]
fn sync_directory(path: &Path) -> Result<(), UriCallbackError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(UriCallbackError::Io)
}

/// Validates an RFC 3986 scheme that is not a reserved web scheme.
fn validate_scheme(scheme: &str) -> Result<(), UriCallbackError> {
    if scheme.is_empty() || scheme.len() > MAXIMUM_URI_SCHEME_BYTES {
        return Err(UriCallbackError::InvalidScheme);
    }
    let mut characters = scheme.chars();
    let first = characters.next().ok_or(UriCallbackError::InvalidScheme)?;
    if !first.is_ascii_lowercase() {
        return Err(UriCallbackError::InvalidScheme);
    }
    if !characters.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '+' | '-' | '.')
    }) {
        return Err(UriCallbackError::InvalidScheme);
    }
    // A handler for these would intercept ordinary web navigation rather than one private
    // redirect, so the loopback listener owns those flows instead.
    if matches!(scheme, "http" | "https" | "file" | "ftp") {
        return Err(UriCallbackError::ReservedScheme);
    }
    Ok(())
}

/// Validates the post-scheme remainder as RFC 3986 path/authority syntax.
///
/// A `%` must introduce exactly two hexadecimal digits; otherwise the base could be
/// registered here yet rejected or normalized differently by the authorization server.
fn validate_uri_remainder(remainder: &str) -> Result<(), UriCallbackError> {
    if remainder.is_empty() {
        return Err(UriCallbackError::InvalidRedirectUri);
    }
    let bytes = remainder.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !bytes
                .get(index + 1..index + 3)
                .is_some_and(|escape| escape.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(UriCallbackError::InvalidRedirectUri);
        }
    }
    if remainder.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(
                character,
                '/' | '-'
                    | '.'
                    | '_'
                    | '~'
                    | '%'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | ':'
                    | '@'
                    | '['
                    | ']'
            )
    }) {
        Ok(())
    } else {
        Err(UriCallbackError::InvalidRedirectUri)
    }
}

#[cfg(target_os = "linux")]
fn validate_application_id(application_id: &str) -> Result<(), UriCallbackError> {
    if application_id.is_empty() || application_id.len() > MAXIMUM_APPLICATION_ID_BYTES {
        return Err(UriCallbackError::InvalidApplicationId);
    }
    if application_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        Ok(())
    } else {
        Err(UriCallbackError::InvalidApplicationId)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAXIMUM_REDIRECT_BASE_BYTES, MAXIMUM_REDIRECT_URI_BYTES, NativeUriSchemeRegistrar,
        RegisteredUriRedirect, UriCallbackError,
    };
    use crate::loopback_callback::MAXIMUM_SERVER_ERROR_BYTES;
    use crate::{CapabilityAvailability, PkceSecret};

    #[cfg(target_os = "linux")]
    fn unique_test_directory(kind: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "axiusflow-uri-{kind}-{}-{timestamp}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("test directory is created");
        path
    }

    fn redirect() -> RegisteredUriRedirect {
        RegisteredUriRedirect::try_new("com.axiusflow.terminal:/authorized")
            .expect("private redirect URI is valid")
    }

    #[test]
    fn redirect_uri_validation_rejects_reserved_and_malformed_schemes() {
        assert_eq!(redirect().scheme(), "com.axiusflow.terminal");
        assert!(matches!(
            RegisteredUriRedirect::try_new("https:/authorized"),
            Err(UriCallbackError::ReservedScheme)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("1nvalid:/authorized"),
            Err(UriCallbackError::InvalidScheme)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("Com.Example:/authorized"),
            Err(UriCallbackError::InvalidScheme)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:/authorized?code=stolen"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:/authorized path"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:/author\u{7f}ized"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        // A `%` must introduce exactly two hexadecimal digits.
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:/authorized%ZZ"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:/authorized%"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(matches!(
            RegisteredUriRedirect::try_new("com.example:/authorized%2"),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(RegisteredUriRedirect::try_new("com.example:/authorized%2F").is_ok());
        // A base at the hard URI bound can never fit a delivered `?code=...&state=...`.
        assert!(matches!(
            RegisteredUriRedirect::try_new(format!(
                "com.example:/{}",
                "a".repeat(MAXIMUM_REDIRECT_BASE_BYTES)
            )),
            Err(UriCallbackError::InvalidRedirectUri)
        ));
        assert!(
            RegisteredUriRedirect::try_new(format!(
                "com.example:/{}",
                "a".repeat(MAXIMUM_REDIRECT_BASE_BYTES - "com.example:/".len())
            ))
            .is_ok()
        );
    }

    #[test]
    fn verified_redirect_yields_the_authorization_code() {
        let secret = PkceSecret::generate().expect("system CSPRNG is available");
        let delivered = format!(
            "com.axiusflow.terminal:/authorized?code=granted%2Dcode&state={}",
            secret.state()
        );

        let code = redirect()
            .verify_redirect(&delivered, &secret)
            .expect("state-verified redirect yields its code");
        assert_eq!(code.value(), "granted-code");
        assert!(format!("{code:?}").contains("<redacted>"));
    }

    #[test]
    fn unverified_or_foreign_redirects_are_refused() {
        let secret = PkceSecret::generate().expect("system CSPRNG is available");
        let redirect = redirect();

        assert!(matches!(
            redirect.verify_redirect("com.axiusflow.terminal:/authorized?code=granted", &secret),
            Err(UriCallbackError::MissingState)
        ));
        assert!(matches!(
            redirect.verify_redirect(
                "com.axiusflow.terminal:/authorized?code=granted&state=forged",
                &secret
            ),
            Err(UriCallbackError::StateMismatch)
        ));
        assert!(matches!(
            redirect.verify_redirect(
                &format!(
                    "com.axiusflow.terminal:/elsewhere?code=granted&state={}",
                    secret.state()
                ),
                &secret
            ),
            Err(UriCallbackError::RedirectTargetMismatch)
        ));
        assert!(matches!(
            redirect.verify_redirect(
                &format!(
                    "com.axiusflow.terminal:/authorized?state={}",
                    secret.state()
                ),
                &secret
            ),
            Err(UriCallbackError::MissingAuthorizationCode)
        ));
        assert!(matches!(
            redirect.verify_redirect(&"x".repeat(MAXIMUM_REDIRECT_URI_BYTES + 1), &secret),
            Err(UriCallbackError::RedirectUriTooLong)
        ));
    }

    #[test]
    fn state_verified_server_errors_are_sanitized_and_bounded() {
        let secret = PkceSecret::generate().expect("system CSPRNG is available");
        let delivered = format!(
            "com.axiusflow.terminal:/authorized?error={}&state={}",
            "access_denied%20<script>".to_owned() + &"x".repeat(MAXIMUM_SERVER_ERROR_BYTES),
            secret.state()
        );

        match redirect().verify_redirect(&delivered, &secret) {
            Err(UriCallbackError::AuthorizationServerError(error)) => {
                assert!(error.starts_with("access_denied"));
                assert!(!error.contains('<'));
                assert!(error.len() <= MAXIMUM_SERVER_ERROR_BYTES);
            }
            other => panic!("expected a sanitized server error, got {other:?}"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn registration_writes_a_scheme_handler_entry_atomically() {
        use std::fs;

        let data_home = unique_test_directory("entry");

        let entry = NativeUriSchemeRegistrar::write_entry_in(
            &data_home,
            "com.axiusflow.terminal",
            "/opt/axiusflow/terminal",
            &redirect(),
        )
        .expect("scheme handler is registered");
        let contents = fs::read_to_string(&entry).expect("desktop entry is readable");

        assert!(contents.contains("MimeType=x-scheme-handler/com.axiusflow.terminal;"));
        assert!(contents.contains("Exec=\"/opt/axiusflow/terminal\" %u"));
        assert!(
            !fs::read_dir(data_home.join("applications"))
                .expect("applications directory exists")
                .any(|entry| entry
                    .expect("entry is readable")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".partial")),
            "no partial entry is left behind"
        );

        assert!(matches!(
            NativeUriSchemeRegistrar::write_entry_in(
                &data_home,
                "invalid id",
                "/opt/axiusflow/terminal",
                &redirect(),
            ),
            Err(UriCallbackError::InvalidApplicationId)
        ));
        for control in ["/opt/axiusflow/term\ninal", "/opt/axiusflow/term\tinal"] {
            assert!(
                matches!(
                    NativeUriSchemeRegistrar::write_entry_in(
                        &data_home,
                        "com.axiusflow.terminal",
                        control,
                        &redirect(),
                    ),
                    Err(UriCallbackError::InvalidExecutablePath)
                ),
                "control characters are refused: {control:?}"
            );
        }

        // A path with a space and a field code must stay a single, unexpanded argument.
        let quoted = NativeUriSchemeRegistrar::write_entry_in(
            &data_home,
            "com.axiusflow.spaced",
            "/opt/Axius Flow/100%/terminal",
            &redirect(),
        )
        .expect("a spaced executable path is registrable");
        let quoted_contents = fs::read_to_string(&quoted).expect("desktop entry is readable");
        assert!(quoted_contents.contains("Exec=\"/opt/Axius Flow/100%%/terminal\" %u"));

        // One literal backslash must survive Desktop Entry string parsing *and*
        // command-line parsing, so it is emitted as four backslashes.
        let escaped = NativeUriSchemeRegistrar::write_entry_in(
            &data_home,
            "com.axiusflow.escaped",
            "/opt/axius\\flow/terminal",
            &redirect(),
        )
        .expect("a backslashed executable path is registrable");
        let escaped_contents = fs::read_to_string(&escaped).expect("desktop entry is readable");
        assert!(escaped_contents.contains("Exec=\"/opt/axius\\\\\\\\flow/terminal\" %u"));

        let _ = fs::remove_dir_all(&data_home);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn registration_installs_the_default_handler_and_preserves_other_associations() {
        use std::fs;

        // Private data and config homes: registration must never touch shared host state.
        let data_home = unique_test_directory("data");
        let config_home = unique_test_directory("config");
        fs::write(
            config_home.join("mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/other=keep.desktop\n\n[Added Associations]\nx-scheme-handler/other=keep.desktop;\n",
        )
        .expect("existing associations are created");

        let outcome = NativeUriSchemeRegistrar::register_in(
            &data_home,
            &config_home,
            "com.axiusflow.probe",
            "/opt/axiusflow/terminal",
            &redirect(),
        );
        match outcome {
            Ok(entry) => {
                assert!(entry.is_file(), "registration returns the written entry");
                let associations = fs::read_to_string(config_home.join("mimeapps.list"))
                    .expect("associations are readable");
                assert!(
                    associations.contains(
                        "x-scheme-handler/com.axiusflow.terminal=com.axiusflow.probe.desktop"
                    ),
                    "scheme becomes the default handler: {associations}"
                );
                assert!(
                    associations.contains("x-scheme-handler/other=keep.desktop"),
                    "unrelated associations are preserved: {associations}"
                );
                assert!(
                    associations.contains("[Added Associations]"),
                    "other sections are preserved: {associations}"
                );
                assert!(
                    !fs::read_dir(&config_home)
                        .expect("config home is readable")
                        .any(|entry| entry
                            .expect("entry is readable")
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".partial")),
                    "no partial association file is left behind"
                );
            }
            // Where the host MIME tool is absent, registration must say so explicitly
            // rather than report an inactive handler as registered.
            Err(UriCallbackError::DesktopDatabaseRefreshUnavailable) => {}
            other => panic!("expected registration or an explicit tool failure, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&data_home);
        let _ = fs::remove_dir_all(&config_home);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn concurrent_default_handler_updates_preserve_every_association() {
        use std::fs;
        use std::thread;

        let config_home = unique_test_directory("assoc");
        let schemes = [
            "com.axiusflow.one",
            "com.axiusflow.two",
            "com.axiusflow.three",
        ];
        thread::scope(|scope| {
            for scheme in schemes {
                let config_home = config_home.clone();
                scope.spawn(move || {
                    super::set_default_handler(&config_home, scheme, &format!("{scheme}.desktop"))
                        .expect("association update succeeds under contention");
                });
            }
        });

        let associations =
            fs::read_to_string(config_home.join("mimeapps.list")).expect("associations exist");
        for scheme in schemes {
            assert!(
                associations.contains(&format!("x-scheme-handler/{scheme}={scheme}.desktop")),
                "no concurrent update is lost: {associations}"
            );
        }
        let _ = fs::remove_dir_all(&config_home);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_activation_restores_the_previous_handler() {
        use std::fs;

        let data_home = unique_test_directory("restore");
        let entries = data_home.join("applications");
        fs::create_dir(&entries).expect("applications directory is created");
        let final_path = entries.join("com.axiusflow.restore.desktop");
        fs::write(&final_path, b"[Desktop Entry]\nName=previous\n")
            .expect("a working entry already exists");

        // A config home that is not a directory fails the association step *after* the
        // entry has been replaced, which is exactly the case that must not destroy it.
        let broken_config = data_home.join("not-a-directory");
        fs::write(&broken_config, b"file").expect("broken config path is created");

        let outcome = NativeUriSchemeRegistrar::register_in(
            &data_home,
            &broken_config,
            "com.axiusflow.restore",
            "/opt/axiusflow/terminal",
            &redirect(),
        );
        assert!(outcome.is_err(), "activation failure is reported");
        assert_eq!(
            fs::read(&final_path).expect("previous entry survives"),
            b"[Desktop Entry]\nName=previous\n",
            "the previously working handler is restored"
        );

        // With no previous entry, the newly written one must not be left unreferenced.
        let fresh = unique_test_directory("restore-fresh");
        fs::create_dir(fresh.join("applications")).expect("applications directory is created");
        let fresh_broken = fresh.join("not-a-directory");
        fs::write(&fresh_broken, b"file").expect("broken config path is created");
        assert!(
            NativeUriSchemeRegistrar::register_in(
                &fresh,
                &fresh_broken,
                "com.axiusflow.fresh",
                "/opt/axiusflow/terminal",
                &redirect(),
            )
            .is_err()
        );
        assert!(
            !fresh
                .join("applications")
                .join("com.axiusflow.fresh.desktop")
                .exists(),
            "an entry this call created is removed on failure"
        );

        let _ = fs::remove_dir_all(&data_home);
        let _ = fs::remove_dir_all(&fresh);
    }

    #[test]
    fn availability_matches_the_implemented_native_backend() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            NativeUriSchemeRegistrar::availability(),
            CapabilityAvailability::Available
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            NativeUriSchemeRegistrar::availability(),
            CapabilityAvailability::Unavailable
        );
    }
}
