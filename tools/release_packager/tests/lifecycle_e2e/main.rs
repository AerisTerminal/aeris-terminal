//! End-to-end release lifecycle. The packager builds signed releases whose desktop and launcher
//! are copies of the `aeris_lifecycle_fixture` program, and the launcher installs, updates,
//! checks readiness, activates, holds probation, rolls back and quarantines them from a local
//! directory origin.
//! The desktop's own readiness on a copy of the trading database is covered by the desktop's
//! tests; here the fixture desktop answers with the shared readiness report.

mod directory_source;

use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use aeris_platform_runtime::{
    DataRootMode, initialize_data_root, process::wait_for_processes_to_exit,
};
use aeris_release_lifecycle::{
    ActiveRelease, BLOCK_PLAN_FILENAME, LifecycleError, LiveStoreSchema, RELEASE_MANIFEST_FILENAME,
    ReleaseInstaller, ReleaseOrigin, ReleasePolicy, RolloutMetadata, channel_pointer_path,
    installer_filename, read_live_store_schema, read_startup_complete, run_launcher,
    trading_database_path,
};
use aeris_release_packager::{
    BlockPlanSummary, ChannelRequest, PackagedRelease, ReleaseRequest, desktop_file_name,
    launcher_file_name, package_release, read_signing_key, write_channel_pointer,
};
use aeris_trading_runtime::{
    ExecutionPolicy, TradingRetention, TradingService, TradingServiceConfig, clock::SystemClock,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use directory_source::{DirectorySource, Request};
use ed25519_dalek::SigningKey;

const BASE_URL: &str = "https://releases.example.test/aeris";
const EVENT_TIMEOUT: Duration = Duration::from_secs(60);
/// The event log the fixture writes beside the install root.
const EVENTS_FILE: &str = "fixture-events.log";
/// The trading store schema of every fixture release except the one that migrates past it.
const STORE_SCHEMA: u32 = 30;
/// The launcher's probation bound for a new release's desktop to finish starting.
const STARTUP_COMPLETE_TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn packaged_releases_install_update_roll_back_and_quarantine() {
    let lifecycle = Lifecycle::create();
    lifecycle.run();
    lifecycle.remove();
}

struct Lifecycle {
    base: PathBuf,
    origin: PathBuf,
    install_root: PathBuf,
    data_root: PathBuf,
    key_file: PathBuf,
    key: SigningKey,
    requests: Rc<RefCell<Vec<Request>>>,
}

impl Lifecycle {
    fn create() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let base = long_temporary_directory().join(format!("aeris-lifecycle-e2e-{nanos}"));
        let origin = base.join("origin");
        fs::create_dir_all(&origin).expect("origin directory");
        let key = SigningKey::from_bytes(&[7; 32]);
        let key_file = base.join("release-signing.key");
        fs::write(&key_file, URL_SAFE_NO_PAD.encode(key.to_bytes())).expect("signing key file");
        // The launcher reads the trading store schema below the process data root; a
        // validation root keeps it away from the user's data.
        let data_root = initialize_data_root(DataRootMode::Validation {
            root: base.join("data"),
        })
        .expect("validation data root")
        .path()
        .to_path_buf();
        Self {
            install_root: base.join("Aeris"),
            data_root,
            base,
            origin,
            key_file,
            key,
            requests: Rc::default(),
        }
    }

    fn run(&self) {
        let first = self.install_first_release();
        self.reject_candidate();
        self.crash_and_roll_back(&first);
        let fixed = self.update_after_quarantine(&first);
        self.hang_and_roll_back(&fixed);
        self.refuse_rollback_past_migrated_store(&fixed);
        assert_eq!(self.launcher(&["--recover".into()]).0, Ok(()));
    }

    /// The setup program installs generation 1 from its bundle, then the user launches it.
    fn install_first_release(&self) -> PackagedRelease {
        let first = self.package(1, "e2e-accept-run-0001", None, STORE_SCHEMA);
        assert_eq!(first.block_plan, BlockPlanSummary::NotRequested);
        let manifest = first.directory.join(RELEASE_MANIFEST_FILENAME);
        let (installed, _) = self.launcher(&[
            "--install".into(),
            manifest.into_os_string(),
            first.directory.clone().into_os_string(),
        ]);
        assert_eq!(installed, Ok(()));
        assert_eq!(self.events_of(1, "readiness"), 1);
        assert_eq!(self.releases(), (Some(1), None));
        assert_eq!(self.launcher(&["--launch-desktop".into()]).0, Ok(()));
        self.wait_for_event(1, "launch", 1);
        self.open_trading_store();
        first
    }

    /// A candidate that fails readiness rolls back inside the update. It has no block plan,
    /// so every file downloads in full.
    fn reject_candidate(&self) {
        let rejected = self.package(2, "e2e-reject-run-0002", None, STORE_SCHEMA);
        self.publish(&rejected);
        let (update, requests) = self.launcher(&["--update".into()]);
        assert_eq!(update, Err(LifecycleError::HealthCheckFailed.to_string()));
        assert_eq!(self.events_of(2, "readiness"), 1);
        assert_eq!(self.releases(), (Some(1), None));
        let release = release_path(&rejected);
        assert_eq!(
            requests,
            [
                Request::Channel(channel_path()),
                Request::Object(format!("{release}/{BLOCK_PLAN_FILENAME}")),
                Request::Artifact(format!("{release}/{}", desktop_file_name())),
                Request::Artifact(format!("{release}/{}", launcher_file_name())),
                Request::Artifact(format!("{release}/rollback-compatibility.json")),
            ]
        );
    }

    /// A candidate that passes readiness activates, rebuilt from the active release's blocks
    /// plus the one desktop block that changed. It crashes on its first launch; the retained
    /// release opens the same store schema, so the launcher quarantines it, rolls back and
    /// launches the retained release.
    fn crash_and_roll_back(&self, first: &PackagedRelease) {
        let crashing = self.package(
            3,
            "e2e-accept-crash-0003",
            Some(&first.directory),
            STORE_SCHEMA,
        );
        assert_eq!(
            crashing.block_plan,
            BlockPlanSummary::Written {
                files: 3,
                download_blocks: 1
            }
        );
        self.publish(&crashing);
        let (update, requests) = self.launcher(&["--update".into()]);
        assert_eq!(update, Ok(()));
        let release = release_path(&crashing);
        assert_eq!(
            requests,
            [
                Request::Channel(channel_path()),
                Request::Object(format!("{release}/{BLOCK_PLAN_FILENAME}")),
                Request::Range(format!("{release}/{}", desktop_file_name())),
            ]
        );
        assert_eq!(self.releases(), (Some(3), Some(1)));
        self.wait_for_event(3, "promote", 1);
        self.wait_for_installed_processes();

        assert_eq!(self.launcher(&["--launch-desktop".into()]).0, Ok(()));
        self.wait_for_event(1, "launch", 2);
        assert_eq!(self.events_of(3, "launch"), 1);
        assert_eq!(self.releases(), (Some(1), None));
        assert_eq!(self.quarantined_generation(), 3);
        self.wait_for_installed_processes();
    }

    /// The channel still offers the quarantined release, which the launcher does not take.
    /// A fixed release updates from the restored one, which stays the known-good release
    /// through the new release's first launch.
    fn update_after_quarantine(&self, first: &PackagedRelease) -> PackagedRelease {
        let (update, requests) = self.launcher(&["--update".into()]);
        assert_eq!(update, Ok(()));
        assert_eq!(requests, [Request::Channel(channel_path())]);
        assert_eq!(self.releases(), (Some(1), None));

        let fixed = self.package(
            4,
            "e2e-accept-run-0004",
            Some(&first.directory),
            STORE_SCHEMA,
        );
        self.publish(&fixed);
        assert_eq!(self.launcher(&["--update".into()]).0, Ok(()));
        assert_eq!(self.releases(), (Some(4), Some(1)));
        self.wait_for_event(4, "promote", 1);
        self.wait_for_installed_processes();
        assert_eq!(self.launcher(&["--launch-desktop".into()]).0, Ok(()));
        self.wait_for_event(4, "launch", 1);
        self.wait_for_event(4, "promote", 2);
        assert_eq!(self.releases(), (Some(4), Some(1)));
        assert_eq!(self.startup_complete_generation(), Some(4));
        self.wait_for_installed_processes();
        fixed
    }

    /// A release that never finishes starting is stopped once probation runs out, then
    /// quarantined and rolled back like a crash.
    fn hang_and_roll_back(&self, fixed: &PackagedRelease) {
        let hanging = self.package(
            5,
            "e2e-accept-hang-0005",
            Some(&fixed.directory),
            STORE_SCHEMA,
        );
        self.publish(&hanging);
        assert_eq!(self.launcher(&["--update".into()]).0, Ok(()));
        assert_eq!(self.releases(), (Some(5), Some(4)));
        self.wait_for_event(5, "promote", 1);
        self.wait_for_installed_processes();
        let started = Instant::now();
        assert_eq!(self.launcher(&["--launch-desktop".into()]).0, Ok(()));
        assert!(
            started.elapsed() >= STARTUP_COMPLETE_TIMEOUT,
            "probation ended early"
        );
        assert_eq!(self.events_of(5, "launch"), 1);
        self.wait_for_event(4, "launch", 2);
        assert_eq!(self.releases(), (Some(4), None));
        assert_eq!(self.quarantined_generation(), 5);
        self.wait_for_installed_processes();
    }

    /// A release that migrates the trading store and then crashes cannot roll back to one
    /// that does not open the migrated store: it stays active and is not quarantined.
    fn refuse_rollback_past_migrated_store(&self, fixed: &PackagedRelease) {
        let migrating = self.package(
            6,
            "e2e-accept-crash-0006",
            Some(&fixed.directory),
            STORE_SCHEMA + 1,
        );
        self.publish(&migrating);
        assert_eq!(self.launcher(&["--update".into()]).0, Ok(()));
        assert_eq!(self.releases(), (Some(6), Some(4)));
        self.wait_for_event(6, "promote", 1);
        self.wait_for_installed_processes();
        self.write_trading_store(STORE_SCHEMA + 1);
        let (launch, _) = self.launcher(&["--launch-desktop".into()]);
        let refusal = launch.expect_err("the migrated store blocks the rollback");
        assert!(
            refusal.contains(&LifecycleError::RollbackStoreSchemaNewer.to_string()),
            "{refusal}"
        );
        assert_eq!(self.events_of(6, "launch"), 1);
        assert_eq!(self.releases(), (Some(6), Some(4)));
        assert_eq!(self.quarantined_generation(), 5);
        self.wait_for_installed_processes();
    }

    /// Opens the real trading store the way the released desktop does, so the launcher reads
    /// the schema from a database the trading owner wrote.
    fn open_trading_store(&self) {
        let database = trading_database_path(&self.data_root);
        fs::create_dir_all(database.parent().expect("trading folder")).expect("trading folder");
        let service = TradingService::start(TradingServiceConfig {
            database_path: database.clone(),
            retention: TradingRetention::default(),
            clock: Arc::new(SystemClock::default()),
            execution: ExecutionPolicy::default(),
        })
        .expect("trading owner opens the store");
        service
            .shutdown(EVENT_TIMEOUT)
            .expect("trading owner stops");
        drop(service);
        let live = read_live_store_schema(&database).expect("live store schema");
        assert!(
            matches!(
                live,
                LiveStoreSchema::Checkpointed(STORE_SCHEMA)
                    | LiveStoreSchema::Uncheckpointed(STORE_SCHEMA)
            ),
            "{live:?}"
        );
    }

    /// Writes the trading database header a desktop leaves after migrating the store to
    /// `schema` and checkpointing it.
    fn write_trading_store(&self, schema: u32) {
        let database = trading_database_path(&self.data_root);
        fs::create_dir_all(database.parent().expect("trading folder")).expect("trading folder");
        let mut header = vec![0_u8; 4096];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        header[60..64].copy_from_slice(&schema.to_be_bytes());
        for sidecar in ["-wal", "-shm"] {
            let mut path = database.clone().into_os_string();
            path.push(sidecar);
            match fs::remove_file(PathBuf::from(path)) {
                Ok(()) => {}
                Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
            }
        }
        fs::write(database, header).expect("trading database header");
    }

    fn startup_complete_generation(&self) -> Option<u64> {
        read_startup_complete(&self.install_root)
            .expect("startup-complete marker")
            .map(|marker| marker.install_generation)
    }

    fn quarantined_generation(&self) -> u64 {
        let quarantine: serde_json::Value = serde_json::from_slice(
            &fs::read(self.install_root.join(".release-quarantine.json")).expect("quarantine"),
        )
        .expect("quarantine record");
        quarantine["install_generation"]
            .as_u64()
            .expect("quarantined generation")
    }

    /// Packages one release whose desktop is this executable with a generation trailer, so
    /// consecutive desktops differ only in their last block.
    fn package(
        &self,
        install_generation: u64,
        release_identity: &str,
        previous: Option<&Path>,
        max_store_schema: u32,
    ) -> PackagedRelease {
        let fixture = Path::new(env!("CARGO_BIN_EXE_aeris_lifecycle_fixture"));
        let payload = self
            .base
            .join("payloads")
            .join(install_generation.to_string());
        fs::create_dir_all(&payload).expect("payload directory");
        fs::copy(fixture, payload.join(launcher_file_name())).expect("fixture launcher");
        let mut desktop = fs::read(fixture).expect("fixture desktop");
        desktop.extend_from_slice(format!("\nAeris fixture {install_generation}\n").as_bytes());
        fs::write(payload.join(desktop_file_name()), desktop).expect("fixture desktop");
        package_release(
            &ReleaseRequest {
                payload: &payload,
                output: &self.origin,
                base_url: BASE_URL,
                release_identity,
                install_generation,
                rollout: RolloutMetadata {
                    cohort: "stable".to_string(),
                    percentage: 100,
                },
                max_store_schema,
                previous,
            },
            &read_signing_key(&self.key_file).expect("signing key"),
        )
        .expect("release packages")
    }

    /// Adds the setup program a release build gets from Inno Setup and points the channel at
    /// the release.
    fn publish(&self, release: &PackagedRelease) {
        fs::write(
            release.directory.join(installer_filename()),
            b"fixture setup program",
        )
        .expect("setup program");
        let manifest = release.manifest.manifest();
        write_channel_pointer(&ChannelRequest {
            output: &self.origin,
            base_url: BASE_URL,
            release_identity: &manifest.release_identity,
            install_generation: manifest.install_generation,
            version: "1.2.0",
            published_at: "2026-10-28T12:00:00Z",
        })
        .expect("channel pointer");
    }

    /// Runs one launcher command for the install root and returns its result and the origin
    /// requests it made.
    fn launcher(&self, arguments: &[OsString]) -> (Result<(), String>, Vec<Request>) {
        self.requests.borrow_mut().clear();
        let result = run_launcher(
            arguments.iter().cloned(),
            &self.install_root.join(launcher_file_name()),
            || {
                Ok(ReleaseOrigin {
                    verifying_key: self.key.verifying_key(),
                    base_url: BASE_URL.to_string(),
                    bootstrap_minimum_generation: 1,
                    source: Box::new(DirectorySource::new(
                        &self.origin,
                        BASE_URL,
                        Rc::clone(&self.requests),
                    )),
                })
            },
        );
        (result, self.requests.borrow_mut().drain(..).collect())
    }

    /// The active and retained known-good generations, as an audit of the install sees them.
    fn releases(&self) -> (Option<u64>, Option<u64>) {
        let installer = ReleaseInstaller::new(
            &self.install_root,
            self.key.verifying_key(),
            ReleasePolicy::native(0),
        )
        .expect("installer");
        let generation = |release: Option<ActiveRelease>| release.map(|r| r.install_generation);
        (
            generation(installer.audit_active_release().expect("active release")),
            generation(
                installer
                    .retained_known_good_release()
                    .expect("retained release"),
            ),
        )
    }

    fn events_of(&self, generation: u64, event: &str) -> usize {
        let prefix = format!("{generation} {event}");
        fs::read_to_string(self.base.join(EVENTS_FILE))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == prefix || line.starts_with(&format!("{prefix} ")))
            .count()
    }

    /// Waits for a detached fixture process to record its event.
    fn wait_for_event(&self, generation: u64, event: &str, count: usize) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while self.events_of(generation, event) < count {
            assert!(
                Instant::now() < deadline,
                "generation {generation} did not record {event} {count} times"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Waits until no detached fixture process runs from the install, so the next lifecycle
    /// step can remove a version directory.
    fn wait_for_installed_processes(&self) {
        let versions = self.install_root.join("versions");
        assert!(
            wait_for_processes_to_exit(|image| image.starts_with(&versions), EVENT_TIMEOUT)
                .expect("process list"),
            "an installed fixture process did not exit"
        );
    }

    fn remove(&self) {
        self.wait_for_installed_processes();
        fs::remove_dir_all(&self.base).expect("remove the lifecycle directory");
        let lifecycle_root = self.base.with_file_name(".Aeris-lifecycle");
        assert!(!lifecycle_root.exists());
    }
}

fn channel_path() -> String {
    channel_pointer_path(std::env::consts::OS, std::env::consts::ARCH)
}

fn release_path(release: &PackagedRelease) -> String {
    let manifest = release.manifest.manifest();
    aeris_release_lifecycle::release_directory_path(
        &manifest.platform,
        &manifest.architecture,
        manifest.install_generation,
        &manifest.release_identity,
    )
}

/// The temporary directory with its long path. Process image paths never use 8.3 short
/// names, so a short temporary path would hide the fixtures from process waits.
fn long_temporary_directory() -> PathBuf {
    let directory = fs::canonicalize(std::env::temp_dir()).expect("temporary directory");
    match directory
        .to_str()
        .and_then(|path| path.strip_prefix(r"\\?\"))
    {
        Some(plain) if !plain.starts_with("UNC") => PathBuf::from(plain),
        _ => directory,
    }
}
