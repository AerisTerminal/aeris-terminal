use axiusflow_desktop_provider_runtime::{AuthenticationState, ProviderSessionEvent};
use axiusflow_platform_runtime::{NetworkEvent, PowerEvent};
use axiusflow_rithmic_protocol_adapter::{AppliedRithmicEvent, RithmicEnvironmentEvent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const REPORT_SCHEMA_VERSION: u32 = 1;
const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct EvidenceFlag(bool);

impl EvidenceFlag {
    const fn is_true(self) -> bool {
        self.0
    }
}

impl From<bool> for EvidenceFlag {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct RithmicRuntimeStateEvidence {
    pub(crate) selection_installed: EvidenceFlag,
    pub(crate) instrument_installed: EvidenceFlag,
    pub(crate) history_request_active: EvidenceFlag,
    pub(crate) live_chart_installed: EvidenceFlag,
    pub(crate) pending_live_request: EvidenceFlag,
    pub(crate) buffered_history_trades: usize,
    pub(crate) history_trade_overflow: EvidenceFlag,
    pub(crate) depth_selection_installed: EvidenceFlag,
}

impl RithmicRuntimeStateEvidence {
    const fn is_ready(&self) -> bool {
        self.selection_installed.is_true()
            && self.instrument_installed.is_true()
            && self.live_chart_installed.is_true()
            && !self.history_request_active.is_true()
            && !self.pending_live_request.is_true()
            && self.buffered_history_trades == 0
            && !self.history_trade_overflow.is_true()
            && self.depth_selection_installed.is_true()
    }

    const fn is_cleared(&self) -> bool {
        !self.selection_installed.is_true()
            && !self.instrument_installed.is_true()
            && !self.history_request_active.is_true()
            && !self.live_chart_installed.is_true()
            && !self.pending_live_request.is_true()
            && self.buffered_history_trades == 0
            && !self.history_trade_overflow.is_true()
            && !self.depth_selection_installed.is_true()
    }
}

#[derive(Clone, Debug, Default, Serialize)]
struct PhysicalTransitionEvidence {
    loss_callback_observed: EvidenceFlag,
    loss_source_ordinal: Option<u64>,
    loss_unix_milliseconds: Option<u128>,
    retired_generation: Option<u64>,
    environment_apply_succeeded: EvidenceFlag,
    session_stop_confirmed: EvidenceFlag,
    provider_invalidation_preceded_fence: EvidenceFlag,
    pre_transition_state: Option<RithmicRuntimeStateEvidence>,
    retired_state_cleared: EvidenceFlag,
    post_retirement_state: Option<RithmicRuntimeStateEvidence>,
    restoration_callback_observed: EvidenceFlag,
    restoration_source_ordinal: Option<u64>,
    restoration_unix_milliseconds: Option<u128>,
    fresh_generation: Option<u64>,
    authentication_accepted: EvidenceFlag,
    authentication_unix_milliseconds: Option<u128>,
    runtime_rehydrated: EvidenceFlag,
    rehydrated_unix_milliseconds: Option<u128>,
    restored_runtime_state: Option<RithmicRuntimeStateEvidence>,
    completed: EvidenceFlag,
}

impl PhysicalTransitionEvidence {
    fn observe_loss(
        &mut self,
        source_ordinal: u64,
        retired_generation: Option<u64>,
        session_stop_confirmed: bool,
        provider_invalidation_preceded_fence: bool,
        before: RithmicRuntimeStateEvidence,
        after: RithmicRuntimeStateEvidence,
    ) {
        *self = Self {
            loss_callback_observed: true.into(),
            loss_source_ordinal: Some(source_ordinal),
            loss_unix_milliseconds: Some(unix_milliseconds()),
            retired_generation,
            environment_apply_succeeded: true.into(),
            session_stop_confirmed: session_stop_confirmed.into(),
            provider_invalidation_preceded_fence: provider_invalidation_preceded_fence.into(),
            pre_transition_state: Some(before),
            retired_state_cleared: after.is_cleared().into(),
            post_retirement_state: Some(after),
            ..Self::default()
        };
    }

    fn observe_restoration(&mut self, source_ordinal: u64, fresh_generation: Option<u64>) {
        self.restoration_callback_observed = true.into();
        self.restoration_source_ordinal = Some(source_ordinal);
        self.restoration_unix_milliseconds = Some(unix_milliseconds());
        self.fresh_generation = fresh_generation;
        self.authentication_accepted = false.into();
        self.authentication_unix_milliseconds = None;
        self.runtime_rehydrated = false.into();
        self.rehydrated_unix_milliseconds = None;
        self.restored_runtime_state = None;
        self.recompute();
    }

    fn observe_authentication(&mut self, generation: u64) -> bool {
        if self.fresh_generation != Some(generation) || self.authentication_accepted.is_true() {
            return false;
        }
        self.authentication_accepted = true.into();
        self.authentication_unix_milliseconds = Some(unix_milliseconds());
        self.recompute();
        true
    }

    fn observe_runtime(
        &mut self,
        generation: Option<u64>,
        state: &RithmicRuntimeStateEvidence,
    ) -> bool {
        if self.fresh_generation != generation
            || self.runtime_rehydrated.is_true()
            || !state.is_ready()
        {
            return false;
        }
        self.runtime_rehydrated = true.into();
        self.rehydrated_unix_milliseconds = Some(unix_milliseconds());
        self.restored_runtime_state = Some(state.clone());
        self.recompute();
        true
    }

    fn recompute(&mut self) {
        let generation_advanced = self
            .retired_generation
            .zip(self.fresh_generation)
            .is_some_and(|(retired, fresh)| fresh > retired);
        let ready_before = self
            .pre_transition_state
            .as_ref()
            .is_some_and(RithmicRuntimeStateEvidence::is_ready);
        let callbacks_ordered = self
            .loss_source_ordinal
            .zip(self.restoration_source_ordinal)
            .is_some_and(|(loss, restoration)| restoration > loss);
        self.completed = (self.loss_callback_observed.is_true()
            && self.restoration_callback_observed.is_true()
            && callbacks_ordered
            && generation_advanced
            && ready_before
            && self.session_stop_confirmed.is_true()
            && !self.provider_invalidation_preceded_fence.is_true()
            && self.retired_state_cleared.is_true()
            && self.authentication_accepted.is_true()
            && self.runtime_rehydrated.is_true())
        .into();
    }
}

#[derive(Serialize)]
struct NativeTransitionReport {
    schema_version: u32,
    evidence_scope: &'static str,
    platform: &'static str,
    completion_state: &'static str,
    checkpoint_sequence: u64,
    created_unix_milliseconds: u128,
    checkpoint_unix_milliseconds: u128,
    source_revision: String,
    clean_worktree: EvidenceFlag,
    cargo_lock_path: String,
    cargo_lock_sha256: String,
    executable_path: String,
    executable_sha256: String,
    callback_source: &'static str,
    shipping_mode: &'static str,
    credential_source: &'static str,
    credentials_embedded: EvidenceFlag,
    transitions_triggered_by_capture: EvidenceFlag,
    initial_network_state: &'static str,
    offline_startup_observed: EvidenceFlag,
    observer_overflow: EvidenceFlag,
    observer_overflow_count: u64,
    callbacks_received: u64,
    callbacks_applied: u64,
    callback_application_failures: u64,
    network_offline: PhysicalTransitionEvidence,
    suspend_resume: PhysicalTransitionEvidence,
    scenario_requirements_met: EvidenceFlag,
    worker_clean_stop: EvidenceFlag,
    finalized: EvidenceFlag,
    readiness_qualified: EvidenceFlag,
}

#[derive(Deserialize)]
struct NativeTransitionManifest {
    schema_version: u32,
    evidence_scope: String,
    source_revision: String,
    clean_worktree: bool,
    cargo_lock_path: PathBuf,
    cargo_lock_sha256: String,
    executable_path: PathBuf,
    executable_sha256: String,
    report_path: PathBuf,
    finalized: bool,
    final_report_sha256: Option<String>,
}

pub(crate) struct NativeTransitionCapture {
    report_path: PathBuf,
    report: NativeTransitionReport,
    provider_invalidated_since_ready: bool,
}

pub(crate) struct AppliedEnvironmentEvidence {
    pub(crate) event: RithmicEnvironmentEvent,
    pub(crate) source_ordinal: u64,
    pub(crate) retired_generation: Option<u64>,
    pub(crate) fresh_generation: Option<u64>,
    pub(crate) session_stop_confirmed: bool,
    pub(crate) before: RithmicRuntimeStateEvidence,
    pub(crate) after: RithmicRuntimeStateEvidence,
}

impl NativeTransitionCapture {
    pub(crate) fn start(report_path: PathBuf) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if report_path.exists() {
            return Err("native transition report already exists".into());
        }
        let manifest = load_and_validate_manifest(&report_path)?;
        let now = unix_milliseconds();
        let mut capture = Self {
            report_path,
            report: NativeTransitionReport {
                schema_version: REPORT_SCHEMA_VERSION,
                evidence_scope: "rithmic_test_physical_native_transition_capture",
                platform: std::env::consts::OS,
                completion_state: "incomplete",
                checkpoint_sequence: 0,
                created_unix_milliseconds: now,
                checkpoint_unix_milliseconds: now,
                source_revision: manifest.source_revision,
                clean_worktree: manifest.clean_worktree.into(),
                cargo_lock_path: manifest.cargo_lock_path.display().to_string(),
                cargo_lock_sha256: manifest.cargo_lock_sha256,
                executable_path: manifest.executable_path.display().to_string(),
                executable_sha256: manifest.executable_sha256,
                callback_source: "NativeNetworkMonitor_and_NativePowerMonitor",
                shipping_mode: "rithmic_test_existing_native_vault_worker",
                credential_source: "native_vault",
                credentials_embedded: false.into(),
                transitions_triggered_by_capture: false.into(),
                initial_network_state: "unknown",
                offline_startup_observed: false.into(),
                observer_overflow: false.into(),
                observer_overflow_count: 0,
                callbacks_received: 0,
                callbacks_applied: 0,
                callback_application_failures: 0,
                network_offline: PhysicalTransitionEvidence::default(),
                suspend_resume: PhysicalTransitionEvidence::default(),
                scenario_requirements_met: false.into(),
                worker_clean_stop: false.into(),
                finalized: false.into(),
                readiness_qualified: false.into(),
            },
            provider_invalidated_since_ready: false,
        };
        capture.checkpoint()?;
        Ok(capture)
    }

    pub(crate) fn finalize(
        &mut self,
        worker_clean_stop: bool,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.report.worker_clean_stop = worker_clean_stop.into();
        self.report.finalized = true.into();
        self.recompute_and_checkpoint()
    }

    pub(crate) fn observe_initial_network(
        &mut self,
        initial: Option<NetworkEvent>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.report.initial_network_state = match initial {
            Some(NetworkEvent::Available) => "available",
            Some(NetworkEvent::Unavailable) => "unavailable",
            None => "unavailable_probe",
        };
        self.report.offline_startup_observed = (initial == Some(NetworkEvent::Unavailable)).into();
        self.recompute_and_checkpoint()
    }

    pub(crate) fn observe_environment_applied(
        &mut self,
        evidence: AppliedEnvironmentEvidence,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.report.callbacks_received = self.report.callbacks_received.saturating_add(1);
        self.report.callbacks_applied = self.report.callbacks_applied.saturating_add(1);
        let provider_invalidation_preceded_fence = self.provider_invalidated_since_ready;
        match evidence.event {
            RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable) => {
                self.report.network_offline.observe_loss(
                    evidence.source_ordinal,
                    evidence.retired_generation,
                    evidence.session_stop_confirmed,
                    provider_invalidation_preceded_fence,
                    evidence.before,
                    evidence.after,
                );
            }
            RithmicEnvironmentEvent::Network(NetworkEvent::Available) => self
                .report
                .network_offline
                .observe_restoration(evidence.source_ordinal, evidence.fresh_generation),
            RithmicEnvironmentEvent::Power(PowerEvent::Suspending) => {
                self.report.suspend_resume.observe_loss(
                    evidence.source_ordinal,
                    evidence.retired_generation,
                    evidence.session_stop_confirmed,
                    provider_invalidation_preceded_fence,
                    evidence.before,
                    evidence.after,
                );
            }
            RithmicEnvironmentEvent::Power(PowerEvent::Resumed) => self
                .report
                .suspend_resume
                .observe_restoration(evidence.source_ordinal, evidence.fresh_generation),
        }
        if matches!(
            evidence.event,
            RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable)
                | RithmicEnvironmentEvent::Power(PowerEvent::Suspending)
        ) {
            self.provider_invalidated_since_ready = false;
        }
        self.recompute_and_checkpoint()
    }

    pub(crate) fn observe_environment_failure(
        &mut self,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.report.callbacks_received = self.report.callbacks_received.saturating_add(1);
        self.report.callback_application_failures =
            self.report.callback_application_failures.saturating_add(1);
        self.recompute_and_checkpoint()
    }

    pub(crate) fn observe_overflow(
        &mut self,
        count: u64,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.report.observer_overflow = true.into();
        self.report.observer_overflow_count =
            self.report.observer_overflow_count.saturating_add(count);
        self.recompute_and_checkpoint()
    }

    pub(crate) fn observe_provider_event(
        &mut self,
        event: &AppliedRithmicEvent,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if matches!(
            event,
            AppliedRithmicEvent::Semantic(ProviderSessionEvent::Invalidated { .. })
                | AppliedRithmicEvent::RetryScheduled(_)
                | AppliedRithmicEvent::TerminalFailure { .. }
        ) {
            self.provider_invalidated_since_ready = true;
            return Ok(());
        }
        let AppliedRithmicEvent::Semantic(ProviderSessionEvent::AuthenticationChanged {
            generation,
            state: AuthenticationState::Accepted,
        }) = event
        else {
            return Ok(());
        };
        let generation = generation.get();
        let changed = self
            .report
            .network_offline
            .observe_authentication(generation)
            | self
                .report
                .suspend_resume
                .observe_authentication(generation);
        if changed {
            self.recompute_and_checkpoint()?;
        }
        Ok(())
    }

    pub(crate) fn observe_runtime(
        &mut self,
        generation: Option<u64>,
        state: &RithmicRuntimeStateEvidence,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let changed = self
            .report
            .network_offline
            .observe_runtime(generation, state)
            | self
                .report
                .suspend_resume
                .observe_runtime(generation, state);
        if changed {
            self.provider_invalidated_since_ready = false;
            self.recompute_and_checkpoint()?;
        }
        Ok(())
    }

    fn recompute_and_checkpoint(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.report.scenario_requirements_met = (self.report.callback_application_failures == 0
            && !self.report.observer_overflow.is_true()
            && self.report.network_offline.completed.is_true()
            && self.report.suspend_resume.completed.is_true())
        .into();
        self.report.readiness_qualified = (self.report.scenario_requirements_met.is_true()
            && self.report.finalized.is_true()
            && self.report.worker_clean_stop.is_true())
        .into();
        self.report.completion_state = if self.report.readiness_qualified.is_true() {
            "completed"
        } else {
            "incomplete"
        };
        self.report.checkpoint_sequence = self.report.checkpoint_sequence.saturating_add(1);
        self.report.checkpoint_unix_milliseconds = unix_milliseconds();
        self.checkpoint()
    }

    fn checkpoint(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut contents = serde_json::to_vec_pretty(&self.report)?;
        contents.push(b'\n');
        write_bytes_atomically(&self.report_path, &contents)?;
        Ok(())
    }
}

fn load_and_validate_manifest(
    report_path: &Path,
) -> Result<NativeTransitionManifest, Box<dyn Error + Send + Sync>> {
    let manifest_path = provenance_manifest_path(report_path);
    let manifest: NativeTransitionManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
        || manifest.evidence_scope != "rithmic_test_native_transition_capture_manifest"
        || !is_hex_digest(&manifest.source_revision, 40)
        || !manifest.clean_worktree
        || !is_hex_digest(&manifest.cargo_lock_sha256, 64)
        || !is_hex_digest(&manifest.executable_sha256, 64)
        || manifest.finalized
        || manifest.final_report_sha256.is_some()
    {
        return Err("native transition provenance manifest is invalid".into());
    }
    let executable_path = fs::canonicalize(&manifest.executable_path)?;
    let current_executable = fs::canonicalize(std::env::current_exe()?)?;
    if executable_path != current_executable
        || sha256_file(&executable_path)? != manifest.executable_sha256.to_ascii_uppercase()
    {
        return Err("native transition executable provenance does not match".into());
    }
    let cargo_lock_path = fs::canonicalize(&manifest.cargo_lock_path)?;
    if sha256_file(&cargo_lock_path)? != manifest.cargo_lock_sha256.to_ascii_uppercase() {
        return Err("native transition Cargo.lock provenance does not match".into());
    }
    if absolute_path(&manifest.report_path)? != absolute_path(report_path)? {
        return Err("native transition report path does not match its manifest".into());
    }
    Ok(NativeTransitionManifest {
        cargo_lock_path,
        executable_path,
        ..manifest
    })
}

fn provenance_manifest_path(report_path: &Path) -> PathBuf {
    let mut path = report_path.as_os_str().to_owned();
    path.push(".manifest.json");
    PathBuf::from(path)
}

fn absolute_path(path: &Path) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("{:X}", digest.finalize()))
}

fn is_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unix_milliseconds() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn write_bytes_atomically(
    path: &Path,
    contents: &[u8],
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let temporary_path = temporary_sibling(path)?;
    let result = (|| -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn temporary_sibling(path: &Path) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    let file_name = path
        .file_name()
        .ok_or("native transition report path must name a file")?
        .to_string_lossy();
    Ok(path.with_file_name(format!(".{file_name}.{}.partial", std::process::id())))
}

#[cfg(test)]
mod tests {
    use super::{
        AppliedEnvironmentEvidence, EvidenceFlag, NativeTransitionCapture,
        RithmicRuntimeStateEvidence,
    };
    use axiusflow_desktop_provider_runtime::{
        AuthenticationState, ProviderSessionEvent, SessionGeneration,
    };
    use axiusflow_platform_runtime::{NetworkEvent, PowerEvent};
    use axiusflow_rithmic_protocol_adapter::{AppliedRithmicEvent, RithmicEnvironmentEvent};
    use std::{fs, num::NonZeroU64, path::Path};

    fn generation(value: u64) -> SessionGeneration {
        SessionGeneration::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
    }

    fn ready() -> RithmicRuntimeStateEvidence {
        RithmicRuntimeStateEvidence {
            selection_installed: EvidenceFlag::from(true),
            instrument_installed: EvidenceFlag::from(true),
            history_request_active: EvidenceFlag::from(false),
            live_chart_installed: EvidenceFlag::from(true),
            pending_live_request: EvidenceFlag::from(false),
            buffered_history_trades: 0,
            history_trade_overflow: EvidenceFlag::from(false),
            depth_selection_installed: EvidenceFlag::from(true),
        }
    }

    fn cleared() -> RithmicRuntimeStateEvidence {
        RithmicRuntimeStateEvidence::default()
    }

    fn start_capture(report_path: &Path) -> NativeTransitionCapture {
        let executable_path =
            fs::canonicalize(std::env::current_exe().expect("test executable exists"))
                .expect("test executable canonicalizes");
        let cargo_lock_path =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
                .expect("Cargo.lock canonicalizes");
        let manifest = serde_json::json!({
            "schema_version": 1,
            "evidence_scope": "rithmic_test_native_transition_capture_manifest",
            "source_revision": "0123456789abcdef0123456789abcdef01234567",
            "clean_worktree": true,
            "cargo_lock_path": cargo_lock_path,
            "cargo_lock_sha256": super::sha256_file(&cargo_lock_path).expect("Cargo.lock hashes"),
            "executable_path": executable_path,
            "executable_sha256": super::sha256_file(&executable_path).expect("executable hashes"),
            "report_path": report_path,
            "finalized": false,
            "final_report_sha256": null
        });
        fs::write(
            super::provenance_manifest_path(report_path),
            serde_json::to_vec_pretty(&manifest).expect("manifest serializes"),
        )
        .expect("manifest writes");
        NativeTransitionCapture::start(report_path.to_path_buf())
            .expect("initial checkpoint succeeds")
    }

    fn cleanup(report_path: &Path) {
        let _ = fs::remove_file(report_path);
        let _ = fs::remove_file(super::provenance_manifest_path(report_path));
    }

    fn authenticate(
        capture: &mut NativeTransitionCapture,
        generation_value: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        capture.observe_provider_event(&AppliedRithmicEvent::Semantic(
            ProviderSessionEvent::AuthenticationChanged {
                generation: generation(generation_value),
                state: AuthenticationState::Accepted,
            },
        ))
    }

    #[test]
    fn injected_physical_sequences_complete_only_after_fresh_rehydration() {
        let report_path = std::env::temp_dir().join(format!(
            "axiusflow-native-transitions-{}-{}.json",
            std::process::id(),
            super::unix_milliseconds()
        ));
        let mut capture = start_capture(&report_path);
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
                source_ordinal: 1,
                retired_generation: Some(1),
                fresh_generation: None,
                session_stop_confirmed: true,
                before: ready(),
                after: cleared(),
            })
            .expect("offline checkpoint succeeds");
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Network(NetworkEvent::Available),
                source_ordinal: 2,
                retired_generation: None,
                fresh_generation: Some(2),
                session_stop_confirmed: false,
                before: cleared(),
                after: cleared(),
            })
            .expect("network restoration checkpoint succeeds");
        authenticate(&mut capture, 2).expect("network authentication checkpoint succeeds");
        capture
            .observe_runtime(Some(2), &ready())
            .expect("network rehydration checkpoint succeeds");
        let halfway: serde_json::Value =
            serde_json::from_slice(&fs::read(&report_path).expect("halfway report is readable"))
                .expect("halfway report is JSON");
        assert_eq!(halfway["completion_state"], "incomplete");
        assert_eq!(halfway["network_offline"]["completed"], true);
        assert_eq!(halfway["suspend_resume"]["completed"], false);

        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Power(PowerEvent::Suspending),
                source_ordinal: 3,
                retired_generation: Some(2),
                fresh_generation: None,
                session_stop_confirmed: true,
                before: ready(),
                after: cleared(),
            })
            .expect("suspend checkpoint succeeds");
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Power(PowerEvent::Resumed),
                source_ordinal: 4,
                retired_generation: None,
                fresh_generation: Some(3),
                session_stop_confirmed: false,
                before: cleared(),
                after: cleared(),
            })
            .expect("resume checkpoint succeeds");
        authenticate(&mut capture, 3).expect("resume authentication checkpoint succeeds");
        capture
            .observe_runtime(Some(3), &ready())
            .expect("resume rehydration checkpoint succeeds");
        capture.finalize(true).expect("clean worker stop finalizes");
        let completed: serde_json::Value =
            serde_json::from_slice(&fs::read(&report_path).expect("completed report is readable"))
                .expect("completed report is JSON");
        assert_eq!(completed["completion_state"], "completed");
        assert_eq!(completed["readiness_qualified"], true);
        assert_eq!(completed["transitions_triggered_by_capture"], false);
        assert_eq!(completed["credentials_embedded"], false);
        assert_eq!(completed["network_offline"]["retired_generation"], 1);
        assert_eq!(completed["network_offline"]["fresh_generation"], 2);
        assert_eq!(completed["suspend_resume"]["retired_generation"], 2);
        assert_eq!(completed["suspend_resume"]["fresh_generation"], 3);
        assert_eq!(completed["finalized"], true);
        assert_eq!(completed["worker_clean_stop"], true);
        assert!(
            completed["checkpoint_sequence"]
                .as_u64()
                .is_some_and(|value| value > 8)
        );
        let encoded = serde_json::to_string(&completed).expect("completed report serializes");
        assert!(!encoded.to_ascii_lowercase().contains("password"));
        cleanup(&report_path);
    }

    #[test]
    fn injected_transition_fails_closed_without_ready_and_cleared_state() {
        let report_path = std::env::temp_dir().join(format!(
            "axiusflow-native-transitions-incomplete-{}-{}.json",
            std::process::id(),
            super::unix_milliseconds()
        ));
        let mut capture = start_capture(&report_path);
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
                source_ordinal: 1,
                retired_generation: Some(4),
                fresh_generation: None,
                session_stop_confirmed: true,
                before: cleared(),
                after: ready(),
            })
            .expect("invalid loss still checkpoints");
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Network(NetworkEvent::Available),
                source_ordinal: 2,
                retired_generation: None,
                fresh_generation: Some(5),
                session_stop_confirmed: false,
                before: ready(),
                after: ready(),
            })
            .expect("restoration still checkpoints");
        authenticate(&mut capture, 5).expect("authentication still checkpoints");
        capture
            .observe_runtime(Some(5), &ready())
            .expect("runtime still checkpoints");
        capture
            .finalize(true)
            .expect("incomplete capture finalizes");
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(&report_path).expect("report is readable"))
                .expect("report is JSON");
        assert_eq!(report["completion_state"], "incomplete");
        assert_eq!(report["network_offline"]["completed"], false);
        cleanup(&report_path);
    }

    #[test]
    fn provider_invalidation_before_suspend_makes_the_physical_fence_inconclusive() {
        let report_path = std::env::temp_dir().join(format!(
            "axiusflow-native-transitions-invalidated-{}-{}.json",
            std::process::id(),
            super::unix_milliseconds()
        ));
        let mut capture = start_capture(&report_path);
        capture
            .observe_provider_event(&AppliedRithmicEvent::Semantic(
                ProviderSessionEvent::Invalidated {
                    generation: Some(generation(9)),
                    reason:
                        axiusflow_desktop_provider_runtime::ProviderInvalidationReason::Transport,
                },
            ))
            .expect("provider invalidation is observed");
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Power(PowerEvent::Suspending),
                source_ordinal: 1,
                retired_generation: Some(9),
                fresh_generation: None,
                session_stop_confirmed: true,
                before: ready(),
                after: cleared(),
            })
            .expect("suspend fence checkpoints");
        capture
            .observe_environment_applied(AppliedEnvironmentEvidence {
                event: RithmicEnvironmentEvent::Power(PowerEvent::Resumed),
                source_ordinal: 2,
                retired_generation: None,
                fresh_generation: Some(10),
                session_stop_confirmed: false,
                before: cleared(),
                after: cleared(),
            })
            .expect("resume checkpoints");
        authenticate(&mut capture, 10).expect("authentication checkpoints");
        capture
            .observe_runtime(Some(10), &ready())
            .expect("runtime checkpoints");
        capture
            .finalize(true)
            .expect("inconclusive capture finalizes");
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(&report_path).expect("report is readable"))
                .expect("report is JSON");
        assert_eq!(
            report["suspend_resume"]["provider_invalidation_preceded_fence"],
            true
        );
        assert_eq!(report["suspend_resume"]["completed"], false);
        assert_eq!(report["completion_state"], "incomplete");
        cleanup(&report_path);
    }
}
