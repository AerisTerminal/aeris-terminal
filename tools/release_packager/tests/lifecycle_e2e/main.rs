//! End-to-end release lifecycle. The packager builds signed releases whose desktop and launcher
//! are copies of the `aeris_lifecycle_fixture` program, and the launcher installs, updates,
//! checks readiness, activates, rolls back and quarantines them from a local directory origin.
//! The desktop's own readiness on a copy of the trading database is covered by the desktop's
//! tests; here the fixture desktop answers with the shared readiness report.

mod directory_source;

use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use aeris_platform_runtime::process::wait_for_processes_to_exit;
use aeris_release_lifecycle::{
    ActiveRelease, BLOCK_PLAN_FILENAME, LifecycleError, RELEASE_MANIFEST_FILENAME,
    ReleaseInstaller, ReleaseOrigin, ReleasePolicy, RolloutMetadata, channel_pointer_path,
    installer_filename, run_launcher,
};
use aeris_release_packager::{
    BlockPlanSummary, ChannelRequest, PackagedRelease, ReleaseRequest, desktop_file_name,
    launcher_file_name, package_release, read_signing_key, write_channel_pointer,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use directory_source::{DirectorySource, Request};
use ed25519_dalek::SigningKey;

const BASE_URL: &str = "https://releases.example.test/aeris";
const EVENT_TIMEOUT: Duration = Duration::from_secs(60);
/// The event log the fixture writes beside the install root.
const EVENTS_FILE: &str = "fixture-events.log";

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
        Self {
            install_root: base.join("Aeris"),
            base,
            origin,
            key_file,
            key,
            requests: Rc::default(),
        }
    }

    fn run(&self) {
        // The setup program installs generation 1 from its bundle, then the user launches it.
        let first = self.package(1, "e2e-accept-run-0001", None);
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

        // A candidate that fails readiness rolls back inside the update. It has no block
        // plan, so every file downloads in full.
        let rejected = self.package(2, "e2e-reject-run-0002", None);
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

        // A candidate that passes readiness activates, rebuilt from the active release's
        // blocks plus the one desktop block that changed.
        let crashing = self.package(3, "e2e-accept-crash-0003", Some(&first.directory));
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

        // It crashes on its first launch: the launcher quarantines it, rolls back to the
        // retained release and launches that instead.
        assert_eq!(self.launcher(&["--launch-desktop".into()]).0, Ok(()));
        self.wait_for_event(1, "launch", 2);
        assert_eq!(self.events_of(3, "launch"), 1);
        assert_eq!(self.releases(), (Some(1), None));
        let quarantine: serde_json::Value = serde_json::from_slice(
            &fs::read(self.install_root.join(".release-quarantine.json")).expect("quarantine"),
        )
        .expect("quarantine record");
        assert_eq!(quarantine["install_generation"], 3);
        assert_eq!(quarantine["release_identity"], "e2e-accept-crash-0003");
        self.wait_for_installed_processes();

        // The channel still offers the quarantined release; the launcher does not take it.
        let (update, requests) = self.launcher(&["--update".into()]);
        assert_eq!(update, Ok(()));
        assert_eq!(requests, [Request::Channel(channel_path())]);
        assert_eq!(self.releases(), (Some(1), None));

        // A fixed release updates from the restored one, which stays the known-good release
        // through the new release's first launch.
        let fixed = self.package(4, "e2e-accept-run-0004", Some(&first.directory));
        self.publish(&fixed);
        assert_eq!(self.launcher(&["--update".into()]).0, Ok(()));
        assert_eq!(self.releases(), (Some(4), Some(1)));
        self.wait_for_event(4, "promote", 1);
        self.wait_for_installed_processes();
        assert_eq!(self.launcher(&["--launch-desktop".into()]).0, Ok(()));
        self.wait_for_event(4, "launch", 1);
        self.wait_for_event(4, "promote", 2);
        assert_eq!(self.releases(), (Some(4), Some(1)));
        self.wait_for_installed_processes();
        assert_eq!(self.launcher(&["--recover".into()]).0, Ok(()));
    }

    /// Packages one release whose desktop is this executable with a generation trailer, so
    /// consecutive desktops differ only in their last block.
    fn package(
        &self,
        install_generation: u64,
        release_identity: &str,
        previous: Option<&Path>,
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
