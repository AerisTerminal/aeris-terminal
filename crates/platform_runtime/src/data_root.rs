//! Process-wide data-root selection and single-instance ownership.

use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

/// Selects either the normal per-user data folder or an isolated validation folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataRootMode {
    Production,
    Validation { root: PathBuf },
}

/// Fully resolved process data-root policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataRoot {
    mode: DataRootMode,
    path: PathBuf,
    vault_namespace: String,
}

/// Installed-application side effects forbidden in isolated validation runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledAppSideEffect {
    LauncherPromotion,
    Update,
    NotificationIdentityRegistration,
    StartupCompleteMarker,
}

impl DataRoot {
    #[must_use]
    pub fn mode(&self) -> &DataRootMode {
        &self.mode
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn is_validation(&self) -> bool {
        matches!(self.mode, DataRootMode::Validation { .. })
    }

    #[must_use]
    pub fn vault_namespace(&self) -> &str {
        &self.vault_namespace
    }

    /// Whether the named installed-app side effect is permitted.
    #[must_use]
    pub fn allows_installed_app_side_effect(&self, _side_effect: InstalledAppSideEffect) -> bool {
        !self.is_validation()
    }

    /// Runs an installed-application side effect only when this data root permits it.
    pub fn run_installed_app_side_effect<T>(
        &self,
        side_effect: InstalledAppSideEffect,
        action: impl FnOnce() -> T,
    ) -> Option<T> {
        self.allows_installed_app_side_effect(side_effect)
            .then(action)
    }
}

/// Parsed desktop arguments after validation-only flags have been removed.
#[derive(Debug, Eq, PartialEq)]
pub struct ParsedDataRootArguments {
    pub mode: DataRootMode,
    pub command_arguments: Vec<OsString>,
}

/// Data-root setup failures.
#[derive(Debug)]
pub enum DataRootError {
    InvalidArguments(&'static str),
    ProductionRootUnavailable,
    ValidationRootMustBeAbsolute,
    ValidationRootOverlapsProduction,
    AlreadyInitialized,
    RootUnavailable,
    AlreadyRunning,
    UnsafeOwnedRoot,
    StateMigrationFailed,
}

impl std::fmt::Display for DataRootError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArguments(message) => message,
            Self::ProductionRootUnavailable => "the production data folder is unavailable",
            Self::ValidationRootMustBeAbsolute => "the validation root must be an absolute path",
            Self::ValidationRootOverlapsProduction => {
                "the validation data folder must not equal or contain the production data folder"
            }
            Self::AlreadyInitialized => "the process data folder was already initialized",
            Self::RootUnavailable => "the selected data folder is unavailable",
            Self::AlreadyRunning => "Aeris is already running for this data folder",
            Self::UnsafeOwnedRoot => "an Aeris-owned folder path is unsafe to own",
            Self::StateMigrationFailed => "legacy local state migration did not complete",
        })
    }
}

impl std::error::Error for DataRootError {}

static ACTIVE_DATA_ROOT: OnceLock<DataRoot> = OnceLock::new();

/// Parses validation flags independently of command ordering.
///
/// # Errors
/// Returns an error for duplicate or incomplete validation flags.
pub fn parse_data_root_arguments(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<ParsedDataRootArguments, DataRootError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    let mut command_arguments = Vec::with_capacity(arguments.len());
    let mut validation_root = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--validation-root" {
            if validation_root.is_some() {
                return Err(DataRootError::InvalidArguments(
                    "--validation-root may only be supplied once",
                ));
            }
            index += 1;
            let root = arguments.get(index).ok_or(DataRootError::InvalidArguments(
                "--validation-root requires a directory",
            ))?;
            if root.to_string_lossy().starts_with("--") {
                return Err(DataRootError::InvalidArguments(
                    "--validation-root requires a directory",
                ));
            }
            validation_root = Some(PathBuf::from(root));
        } else {
            command_arguments.push(arguments[index].clone());
        }
        index += 1;
    }
    let mode = match validation_root {
        Some(root) => DataRootMode::Validation { root },
        None => DataRootMode::Production,
    };
    Ok(ParsedDataRootArguments {
        mode,
        command_arguments,
    })
}

/// Resolves and installs the process-wide data-root policy.
///
/// # Errors
/// Returns an error when the selected root is unsafe or unavailable, or when
/// the process policy was already initialized.
pub fn initialize_data_root(mode: DataRootMode) -> Result<&'static DataRoot, DataRootError> {
    let production = native_production_data_root()?;
    let resolved = resolve_data_root(mode, &production)?;
    ACTIVE_DATA_ROOT
        .set(resolved)
        .map_err(|_| DataRootError::AlreadyInitialized)?;
    active_data_root()
}

/// Returns the initialized process policy.
///
/// # Errors
/// Returns an error before [`initialize_data_root`] has installed the policy.
pub fn active_data_root() -> Result<&'static DataRoot, DataRootError> {
    ACTIVE_DATA_ROOT
        .get()
        .ok_or(DataRootError::ProductionRootUnavailable)
}

fn resolve_data_root(mode: DataRootMode, production: &Path) -> Result<DataRoot, DataRootError> {
    match mode {
        DataRootMode::Production => Ok(DataRoot {
            mode: DataRootMode::Production,
            path: production.to_path_buf(),
            vault_namespace: "production".to_string(),
        }),
        DataRootMode::Validation { root } => {
            if !root.is_absolute() {
                return Err(DataRootError::ValidationRootMustBeAbsolute);
            }
            let canonical_base = canonical_or_lexical_absolute(&root)?;
            let path = canonical_or_lexical_absolute(&canonical_base.join("Aeris"))?;
            let production = canonical_or_lexical_absolute(production)?;
            if production == path || production.starts_with(&path) || path.starts_with(&production)
            {
                return Err(DataRootError::ValidationRootOverlapsProduction);
            }
            let digest = canonical_root_digest(&path);
            let suffix = digest[..6]
                .iter()
                .fold(String::with_capacity(12), |mut output, byte| {
                    let _ = write!(output, "{byte:02x}");
                    output
                });
            let vault_namespace = format!("com.aeris.terminal.validation.{suffix}");
            Ok(DataRoot {
                mode: DataRootMode::Validation {
                    root: canonical_base,
                },
                path,
                vault_namespace,
            })
        }
    }
}

/// Hashes the canonical or lexically normalized absolute selected data root
/// (including its terminal `Aeris` component) in native OS-string encoding.
fn canonical_root_digest(path: &Path) -> [u8; 32] {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.finalize().into()
}

fn canonical_or_lexical_absolute(path: &Path) -> Result<PathBuf, DataRootError> {
    if let Ok(path) = fs::canonicalize(path) {
        return Ok(path);
    }
    if !path.is_absolute() {
        return Err(DataRootError::ProductionRootUnavailable);
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    while let Some(name) = ancestor.file_name() {
        missing.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
        if let Ok(mut canonical) = fs::canonicalize(ancestor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return Ok(canonical);
        }
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Holds the exclusive advisory lock for one selected data root.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Acquires `<data-root>/instance.lock` without waiting.
    ///
    /// # Errors
    /// Returns [`DataRootError::AlreadyRunning`] when another process owns the
    /// root, or [`DataRootError::RootUnavailable`] for filesystem failures.
    pub fn acquire(root: &DataRoot) -> Result<Self, DataRootError> {
        fs::create_dir_all(root.path()).map_err(|_| DataRootError::RootUnavailable)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.path().join("instance.lock"))
            .map_err(|_| DataRootError::RootUnavailable)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => DataRootError::AlreadyRunning,
            std::fs::TryLockError::Error(_) => DataRootError::RootUnavailable,
        })?;
        Ok(Self { _file: file })
    }
}

/// Resolves the platform production data directory without consulting validation mode.
///
/// # Errors
/// Returns an error when the platform's per-user data directory is unavailable.
pub fn native_production_data_root() -> Result<PathBuf, DataRootError> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("Aeris"))
            .ok_or(DataRootError::ProductionRootUnavailable)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Library/Application Support/Aeris"))
            .ok_or(DataRootError::ProductionRootUnavailable)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".local/share"))
            })
            .map(|root| root.join("aeris"))
            .ok_or(DataRootError::ProductionRootUnavailable)
    }
}

/// Resolves the active process data root.
///
/// # Errors
/// Returns an error when no platform production root is available and no policy
/// has been initialized.
pub fn native_data_root() -> Result<PathBuf, DataRootError> {
    if let Some(root) = ACTIVE_DATA_ROOT.get() {
        return Ok(root.path.clone());
    }
    native_production_data_root()
}

/// Resolves the per-user data folder an earlier product name used.
///
/// # Errors
/// Returns an error when the platform's per-user data directory is unavailable.
pub fn native_legacy_data_root(product: &str) -> Result<PathBuf, DataRootError> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join(product))
            .ok_or(DataRootError::ProductionRootUnavailable)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|root| root.join("Library/Application Support").join(product))
            .ok_or(DataRootError::ProductionRootUnavailable)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".local/share"))
            })
            .map(|root| root.join(product.to_ascii_lowercase()))
            .ok_or(DataRootError::ProductionRootUnavailable)
    }
}

/// Migrates legacy product roots only in production mode.
///
/// # Errors
/// Returns an error when owned roots are unsafe or migration cannot complete.
pub fn migrate_legacy_native_data_root() -> Result<(), DataRootError> {
    if ACTIVE_DATA_ROOT.get().is_some_and(DataRoot::is_validation) {
        return Ok(());
    }
    let current = native_data_root()?;
    validate_owned_root(&current)?;
    for product in ["TradingPlot", "Axiusflow"] {
        let legacy = native_legacy_data_root(product)?;
        if legacy == current || !legacy.exists() {
            continue;
        }
        validate_owned_root(&legacy)?;
        if let Ok(metadata) = fs::symlink_metadata(&current)
            && (!metadata.is_dir() || metadata.file_type().is_symlink())
        {
            return Err(DataRootError::StateMigrationFailed);
        }
        merge_legacy_owned_directory(&legacy, &current)?;
    }
    Ok(())
}

/// Accepts only an exact Aeris-owned folder: absolute, at least three components deep,
/// not a broad per-user or temporary root, and without a symbolic link on its path.
///
/// # Errors
/// Returns [`DataRootError::UnsafeOwnedRoot`] when the path could reach outside Aeris's data.
pub fn validate_owned_root(path: &Path) -> Result<(), DataRootError> {
    if !path.is_absolute()
        || path.components().count() < 3
        || path.parent().is_none()
        || path == Path::new("/")
        || broad_native_root(path)
    {
        return Err(DataRootError::UnsafeOwnedRoot);
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DataRootError::UnsafeOwnedRoot);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(DataRootError::UnsafeOwnedRoot),
        }
    }
    Ok(())
}

fn broad_native_root(path: &Path) -> bool {
    if path == std::env::temp_dir() {
        return true;
    }
    for variable in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
    ] {
        if std::env::var_os(variable).is_some_and(|root| path.as_os_str() == root) {
            return true;
        }
    }
    std::env::var_os("HOME").is_some_and(|home| {
        let home = PathBuf::from(home);
        [
            home.join(".local/share"),
            home.join(".cache"),
            home.join("Library/Application Support"),
            home.join("Library/Caches"),
        ]
        .iter()
        .any(|root| path == root)
    })
}

fn merge_legacy_owned_directory(source: &Path, destination: &Path) -> Result<(), DataRootError> {
    let source_metadata =
        fs::symlink_metadata(source).map_err(|_| DataRootError::StateMigrationFailed)?;
    if !source_metadata.is_dir() || source_metadata.file_type().is_symlink() {
        return Err(DataRootError::StateMigrationFailed);
    }
    fs::create_dir_all(destination).map_err(|_| DataRootError::StateMigrationFailed)?;
    let destination_metadata =
        fs::symlink_metadata(destination).map_err(|_| DataRootError::StateMigrationFailed)?;
    if !destination_metadata.is_dir() || destination_metadata.file_type().is_symlink() {
        return Err(DataRootError::StateMigrationFailed);
    }
    for entry in fs::read_dir(source).map_err(|_| DataRootError::StateMigrationFailed)? {
        let entry = entry.map_err(|_| DataRootError::StateMigrationFailed)?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let source_type = entry
            .file_type()
            .map_err(|_| DataRootError::StateMigrationFailed)?;
        if source_type.is_symlink() {
            return Err(DataRootError::StateMigrationFailed);
        }
        match fs::symlink_metadata(&destination_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::rename(&source_path, &destination_path)
                    .map_err(|_| DataRootError::StateMigrationFailed)?;
            }
            Ok(destination_metadata)
                if source_type.is_dir()
                    && destination_metadata.is_dir()
                    && !destination_metadata.file_type().is_symlink() =>
            {
                merge_legacy_owned_directory(&source_path, &destination_path)?;
            }
            Ok(_) => {}
            Err(_) => return Err(DataRootError::StateMigrationFailed),
        }
    }
    let mut remaining = fs::read_dir(source).map_err(|_| DataRootError::StateMigrationFailed)?;
    let empty = remaining.next().is_none();
    drop(remaining);
    if empty {
        fs::remove_dir(source).map_err(|_| DataRootError::StateMigrationFailed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("aeris-data-root-{name}-{nonce}"));
        fs::create_dir_all(&root).expect("temporary root");
        root
    }

    #[test]
    fn parser_accepts_validation_flags_around_commands() {
        let parsed = parse_data_root_arguments([
            "--desktop-readiness".into(),
            "report.json".into(),
            "--validation-root".into(),
            "C:\\validation".into(),
        ])
        .expect("arguments");
        assert_eq!(
            parsed.command_arguments,
            [
                OsString::from("--desktop-readiness"),
                OsString::from("report.json")
            ]
        );
        assert!(matches!(parsed.mode, DataRootMode::Validation { .. }));
    }

    #[test]
    fn validation_rejects_relative_and_production_containing_roots() {
        let production = temp_root("production").join("Aeris");
        assert!(matches!(
            resolve_data_root(
                DataRootMode::Validation {
                    root: PathBuf::from("relative"),
                },
                &production
            ),
            Err(DataRootError::ValidationRootMustBeAbsolute)
        ));
        let parent = production.parent().expect("parent");
        assert!(matches!(
            resolve_data_root(
                DataRootMode::Validation {
                    root: parent.to_path_buf(),
                },
                &production
            ),
            Err(DataRootError::ValidationRootOverlapsProduction)
        ));
        let nested_production = parent.join("validation").join("Aeris").join("nested");
        assert!(matches!(
            resolve_data_root(
                DataRootMode::Validation {
                    root: parent.join("validation"),
                },
                &nested_production
            ),
            Err(DataRootError::ValidationRootOverlapsProduction)
        ));
        let absent = parent.join("new-validation-root");
        let resolved = resolve_data_root(
            DataRootMode::Validation {
                root: absent.clone(),
            },
            &parent.join("production").join("Aeris"),
        )
        .expect("an absolute validation root may be created on first use");
        assert_eq!(
            resolved.path(),
            canonical_or_lexical_absolute(&absent.join("Aeris")).expect("expected validation path")
        );
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn instance_lock_contends_reacquires_and_is_scoped_per_root() {
        let first_parent = temp_root("lock-first");
        let second_parent = temp_root("lock-second");
        let first = resolve_data_root(
            DataRootMode::Validation {
                root: first_parent.clone(),
            },
            &temp_root("lock-production").join("Aeris"),
        )
        .expect("first root");
        let second = resolve_data_root(
            DataRootMode::Validation {
                root: second_parent.clone(),
            },
            &temp_root("lock-production-two").join("Aeris"),
        )
        .expect("second root");
        let lock = InstanceLock::acquire(&first).expect("first lock");
        assert!(matches!(
            InstanceLock::acquire(&first),
            Err(DataRootError::AlreadyRunning)
        ));
        let other = InstanceLock::acquire(&second).expect("second root lock");
        drop(lock);
        let reacquired = InstanceLock::acquire(&first).expect("reacquired");
        drop((reacquired, other));
        fs::remove_dir_all(first_parent).expect("first cleanup");
        fs::remove_dir_all(second_parent).expect("second cleanup");
    }

    #[test]
    fn validation_disables_all_installed_app_side_effect_actions() {
        let validation_parent = temp_root("side-effects");
        let validation = resolve_data_root(
            DataRootMode::Validation {
                root: validation_parent.clone(),
            },
            &temp_root("side-effects-production").join("Aeris"),
        )
        .expect("validation root");
        let mut validation_invocations = 0_u8;
        for side_effect in [
            InstalledAppSideEffect::LauncherPromotion,
            InstalledAppSideEffect::Update,
            InstalledAppSideEffect::NotificationIdentityRegistration,
            InstalledAppSideEffect::StartupCompleteMarker,
        ] {
            assert!(!validation.allows_installed_app_side_effect(side_effect));
            assert_eq!(
                validation.run_installed_app_side_effect(side_effect, || {
                    validation_invocations += 1;
                }),
                None
            );
        }
        assert_eq!(validation_invocations, 0);
        let digest = canonical_root_digest(validation.path());
        let suffix = digest[..6].iter().fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        });
        assert_eq!(
            validation.vault_namespace(),
            format!("com.aeris.terminal.validation.{suffix}")
        );
        let production = resolve_data_root(
            DataRootMode::Production,
            &validation_parent.join("production"),
        )
        .expect("production root");
        let mut production_invocations = 0_u8;
        for side_effect in [
            InstalledAppSideEffect::LauncherPromotion,
            InstalledAppSideEffect::Update,
            InstalledAppSideEffect::NotificationIdentityRegistration,
            InstalledAppSideEffect::StartupCompleteMarker,
        ] {
            assert!(production.allows_installed_app_side_effect(side_effect));
            assert_eq!(
                production.run_installed_app_side_effect(side_effect, || {
                    production_invocations += 1;
                }),
                Some(())
            );
        }
        assert_eq!(production_invocations, 4);
        fs::remove_dir_all(validation_parent).expect("cleanup");
    }

    #[test]
    fn legacy_state_merge_preserves_current_files_and_moves_missing_state() {
        let root = temp_root("legacy-state-merge");
        let legacy = root.join("Axiusflow");
        let current = root.join("Aeris");
        fs::create_dir_all(legacy.join("desktop")).expect("legacy desktop directory");
        fs::create_dir_all(&current).expect("current state directory");
        fs::write(legacy.join("workspace-state.pb"), b"legacy-workspace")
            .expect("legacy workspace");
        fs::write(current.join("workspace-state.pb"), b"current-workspace")
            .expect("current workspace");
        fs::write(legacy.join("desktop/chart-chrome"), b"legacy-chart-chrome")
            .expect("legacy chart chrome");

        merge_legacy_owned_directory(&legacy, &current).expect("legacy state merges");

        assert_eq!(
            fs::read(current.join("workspace-state.pb")).expect("current workspace reads"),
            b"current-workspace"
        );
        assert_eq!(
            fs::read(current.join("desktop/chart-chrome")).expect("migrated chart chrome reads"),
            b"legacy-chart-chrome"
        );
        assert_eq!(
            fs::read(legacy.join("workspace-state.pb")).expect("conflicting legacy state remains"),
            b"legacy-workspace"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tradingplot_state_merges_without_replacing_newer_aeris_state() {
        let root = temp_root("tradingplot-migration");
        let previous = root.join("TradingPlot");
        let current = root.join("Aeris");
        fs::create_dir_all(&previous).expect("previous state directory");
        fs::create_dir_all(&current).expect("current state directory");
        fs::write(previous.join("workspace-state.pb"), b"previous").expect("previous workspace");
        fs::write(current.join("workspace-state.pb"), b"current").expect("current workspace");
        fs::write(previous.join("chart-chrome"), b"previous-chrome").expect("previous preferences");

        merge_legacy_owned_directory(&previous, &current).expect("state merges");
        assert_eq!(
            fs::read(current.join("workspace-state.pb")).expect("current workspace reads"),
            b"current"
        );
        assert_eq!(
            fs::read(current.join("chart-chrome")).expect("preferences read"),
            b"previous-chrome"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
