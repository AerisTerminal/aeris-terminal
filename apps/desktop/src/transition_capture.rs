//! Physical native-transition capture recorder.
//!
//! This module owns the evidence state machine for the Rithmic
//! offline-startup, network-offline, and suspend/resume capture. It is
//! deliberately free of UI, monitor, and worker handles: the capture
//! command feeds it observed facts (monitor callbacks, provider
//! loss/restoration milestones, ready-state snapshots), and it derives
//! phases, assigns capture-local source ordinals, and validates the
//! complete report against the same rules the external verifier enforces.
//!
//! Source ordinals are capture-local sequence numbers over observed
//! provider loss/restoration callbacks, starting at one. The verifier
//! requires them strictly increasing across the physical sequence with
//! chained retired/fresh generations; it does not interpret them beyond
//! that, and neither do we.

use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{fmt, fs};

use axiusflow_engine_protocol::{
    InstallProviderInstrument, SearchProviderInstruments, SeriesCadence, SeriesKey, envelope,
};
use axiusflow_local_engine_client::EngineClient;
use axiusflow_platform_runtime::{
    NativeNetworkMonitor, NativePowerMonitor, NetworkEvent, PowerEvent,
};

/// Schema version pinned by the external verifier.
pub const REPORT_SCHEMA_VERSION: u32 = 2;
/// Evidence scope pinned by the external verifier.
pub const REPORT_EVIDENCE_SCOPE: &str = "rithmic_test_physical_native_transition_capture";
/// Monitor surface pinned by the external verifier.
pub const REPORT_CALLBACK_SOURCE: &str = "NativeNetworkMonitor_and_NativePowerMonitor";
/// Shipping path pinned by the external verifier: the existing Rithmic
/// worker backed by native-vault credentials, never a parallel harness.
pub const REPORT_SHIPPING_MODE: &str = "rithmic_test_existing_native_vault_worker";
/// Credential origin pinned by the external verifier.
pub const REPORT_CREDENTIAL_SOURCE: &str = "native_vault";

/// One applied native-monitor observation. Network availability changes
/// bracket the offline and network phases; suspend/resume brackets the
/// power phase. Phases are attributed by provider loss/restoration
/// milestones, with monitor windows validated at finalize time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorObservation {
    NetworkUnavailable,
    NetworkAvailable,
    PowerSuspending,
    PowerResumed,
}

/// Ready-state snapshot asserted at every restoration point.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
pub struct ReadySnapshot {
    pub selection_installed: bool,
    pub instrument_installed: bool,
    pub history_request_active: bool,
    pub live_chart_installed: bool,
    pub pending_live_request: bool,
    pub buffered_history_trades: u64,
    pub history_trade_overflow: bool,
    pub depth_selection_installed: bool,
}

impl ReadySnapshot {
    /// The only ready state the capture accepts: everything installed,
    /// nothing in flight, nothing buffered, nothing overflowed.
    #[must_use]
    pub const fn installed() -> Self {
        Self {
            selection_installed: true,
            instrument_installed: true,
            history_request_active: false,
            live_chart_installed: true,
            pending_live_request: false,
            buffered_history_trades: 0,
            history_trade_overflow: false,
            depth_selection_installed: true,
        }
    }

    fn validate(&self, name: &str, errors: &mut Vec<String>) {
        for (property, ok) in [
            ("selection_installed", self.selection_installed),
            ("instrument_installed", self.instrument_installed),
            ("live_chart_installed", self.live_chart_installed),
            ("depth_selection_installed", self.depth_selection_installed),
        ] {
            if !ok {
                errors.push(format!("{name}.{property} must be true"));
            }
        }
        for (property, ok) in [
            ("history_request_active", !self.history_request_active),
            ("pending_live_request", !self.pending_live_request),
            ("history_trade_overflow", !self.history_trade_overflow),
        ] {
            if !ok {
                errors.push(format!("{name}.{property} must be false"));
            }
        }
        if self.buffered_history_trades != 0 {
            errors.push(format!("{name}.buffered_history_trades must be empty"));
        }
    }
}

/// Cleared-state snapshot asserted after every retirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
pub struct ClearedSnapshot {
    pub selection_installed: bool,
    pub instrument_installed: bool,
    pub history_request_active: bool,
    pub live_chart_installed: bool,
    pub pending_live_request: bool,
    pub buffered_history_trades: u64,
    pub history_trade_overflow: bool,
    pub depth_selection_installed: bool,
}

impl ClearedSnapshot {
    /// The only retired state the capture accepts: everything cleared.
    #[must_use]
    pub const fn cleared() -> Self {
        Self {
            selection_installed: false,
            instrument_installed: false,
            history_request_active: false,
            live_chart_installed: false,
            pending_live_request: false,
            buffered_history_trades: 0,
            history_trade_overflow: false,
            depth_selection_installed: false,
        }
    }

    fn validate(&self, name: &str, errors: &mut Vec<String>) {
        for property in [
            "selection_installed",
            "instrument_installed",
            "history_request_active",
            "live_chart_installed",
            "pending_live_request",
            "history_trade_overflow",
            "depth_selection_installed",
        ] {
            let ok = match property {
                "selection_installed" => !self.selection_installed,
                "instrument_installed" => !self.instrument_installed,
                "history_request_active" => !self.history_request_active,
                "live_chart_installed" => !self.live_chart_installed,
                "pending_live_request" => !self.pending_live_request,
                "history_trade_overflow" => !self.history_trade_overflow,
                "depth_selection_installed" => !self.depth_selection_installed,
                _ => true,
            };
            if !ok {
                errors.push(format!("{name}.{property} must be false"));
            }
        }
        if self.buffered_history_trades != 0 {
            errors.push(format!("{name}.buffered_history_trades must be empty"));
        }
    }
}

/// Offline-startup recovery record.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
pub struct StartupRecovery {
    pub restoration_callback_observed: bool,
    pub authentication_accepted: bool,
    pub runtime_rehydrated: bool,
    pub completed: bool,
    pub restoration_source_ordinal: u64,
    pub fresh_generation: u64,
    pub restoration_unix_milliseconds: u64,
    pub authentication_unix_milliseconds: u64,
    pub rehydrated_unix_milliseconds: u64,
    pub restored_runtime_state: ReadySnapshot,
}

/// One network-offline or suspend/resume transition record.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
pub struct TransitionRecord {
    pub loss_callback_observed: bool,
    pub environment_apply_succeeded: bool,
    pub session_stop_confirmed: bool,
    pub retired_state_cleared: bool,
    pub restoration_callback_observed: bool,
    pub authentication_accepted: bool,
    pub runtime_rehydrated: bool,
    pub completed: bool,
    pub provider_invalidation_preceded_fence: bool,
    pub loss_source_ordinal: u64,
    pub restoration_source_ordinal: u64,
    pub retired_generation: u64,
    pub fresh_generation: u64,
    pub loss_unix_milliseconds: u64,
    pub restoration_unix_milliseconds: u64,
    pub authentication_unix_milliseconds: u64,
    pub rehydrated_unix_milliseconds: u64,
    pub pre_transition_state: ReadySnapshot,
    pub post_retirement_state: ClearedSnapshot,
    pub restored_runtime_state: ReadySnapshot,
}

/// Provenance captured at recorder creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureProvenance {
    pub source_revision: String,
    pub clean_worktree: bool,
    pub cargo_lock_path: String,
    pub cargo_lock_sha256: String,
    pub executable_path: String,
    pub executable_sha256: String,
}

/// Complete capture report in the verifier's schema.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
pub struct CaptureReport {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub platform: String,
    pub completion_state: String,
    pub source_revision: String,
    pub clean_worktree: bool,
    pub cargo_lock_path: String,
    pub cargo_lock_sha256: String,
    pub executable_path: String,
    pub executable_sha256: String,
    pub callback_source: String,
    pub shipping_mode: String,
    pub credential_source: String,
    pub credentials_embedded: bool,
    pub transitions_triggered_by_capture: bool,
    pub observer_overflow: bool,
    pub observer_overflow_count: u64,
    pub monitor_failures: u64,
    pub callback_application_failures: u64,
    pub callbacks_received: u64,
    pub callbacks_applied: u64,
    pub initial_network_state: String,
    pub offline_startup_observed: bool,
    pub created_unix_milliseconds: u64,
    pub checkpoint_unix_milliseconds: u64,
    pub offline_startup_recovery: StartupRecovery,
    pub network_offline: TransitionRecord,
    pub suspend_resume: TransitionRecord,
    pub scenario_requirements_met: bool,
    pub worker_clean_stop: bool,
    pub finalized: bool,
    pub readiness_qualified: bool,
}

/// Actionable capture failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureFailure(pub Vec<String>);

impl fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "transition capture is incomplete: {}",
            self.0.join("; ")
        )
    }
}

impl std::error::Error for CaptureFailure {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    AwaitingOfflineStart,
    StartupRecovery,
    AwaitingNetworkLoss,
    NetworkRecovery,
    AwaitingPowerLoss,
    PowerRecovery,
    Complete,
}

/// Evidence state machine for one physical capture.
///
/// The machine never synthesizes transitions: it only records observed
/// monitor callbacks and provider milestones, assigns capture-local source
/// ordinals, and validates the complete physical sequence at finalize time.
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
pub struct TransitionRecorder {
    provenance: CaptureProvenance,
    created_unix_milliseconds: u64,
    next_ordinal: u64,
    phase: Phase,
    callbacks_received: u64,
    callbacks_applied: u64,
    observer_overflow_count: u64,
    monitor_failures: u64,
    callback_application_failures: u64,
    initial_network_observed: bool,
    loss_open: bool,
    startup: StartupBuilder,
    network: TransitionBuilder,
    power: TransitionBuilder,
    scratch_directory: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug, Default)]
struct StartupBuilder {
    restoration_observed: bool,
    restoration_ordinal: u64,
    fresh_generation: u64,
    restoration_ms: u64,
    authenticated: bool,
    authentication_ms: u64,
    rehydrated: bool,
    rehydrated_ms: u64,
    restored: Option<ReadySnapshot>,
}

#[derive(Clone, Debug, Default)]
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
struct TransitionBuilder {
    loss_observed: bool,
    loss_ordinal: u64,
    retired_generation: u64,
    loss_ms: u64,
    environment_applied: bool,
    session_stopped: bool,
    retired_cleared: bool,
    cleared_state: Option<ClearedSnapshot>,
    pre_state: Option<ReadySnapshot>,
    restoration_observed: bool,
    restoration_ordinal: u64,
    fresh_generation: u64,
    restoration_ms: u64,
    authenticated: bool,
    authentication_ms: u64,
    rehydrated: bool,
    rehydrated_ms: u64,
    restored: Option<ReadySnapshot>,
    invalidation_preceded_fence: bool,
}

impl TransitionRecorder {
    /// Starts one capture. The first monitor observation must report the
    /// network unavailable: the capture only proves offline startup.
    #[must_use]
    pub fn new(provenance: CaptureProvenance, created_unix_milliseconds: u64) -> Self {
        Self {
            provenance,
            created_unix_milliseconds,
            next_ordinal: 1,
            phase: Phase::AwaitingOfflineStart,
            callbacks_received: 0,
            callbacks_applied: 0,
            observer_overflow_count: 0,
            monitor_failures: 0,
            callback_application_failures: 0,
            initial_network_observed: false,
            loss_open: false,
            startup: StartupBuilder::default(),
            network: TransitionBuilder::default(),
            power: TransitionBuilder::default(),
            scratch_directory: None,
        }
    }

    /// Attaches a scratch directory receiving a phase log for failed-run
    /// diagnosis. Nothing here enters the evidence report.
    pub fn with_scratch_directory(mut self, directory: std::path::PathBuf) -> Self {
        self.scratch_directory = Some(directory);
        self
    }

    fn take_ordinal(&mut self) -> u64 {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        ordinal
    }

    fn scratch_note(&self, line: &str) {
        if let Some(directory) = &self.scratch_directory {
            let _ = fs::create_dir_all(directory);
            if let Ok(mut file) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("phase.log"))
            {
                use std::io::Write as _;
                let _ = writeln!(file, "{line}");
            }
        }
    }

    /// Applies one native-monitor callback. Every received callback must be
    /// applied: overflow and application failures fail finalization.
    pub fn apply_monitor_callback(&mut self, observation: MonitorObservation, at_ms: u64) {
        self.callbacks_received = self.callbacks_received.saturating_add(1);
        self.callbacks_applied = self.callbacks_applied.saturating_add(1);
        match observation {
            MonitorObservation::NetworkUnavailable => {
                self.initial_network_observed = true;
                self.scratch_note(&format!("monitor network unavailable at {at_ms}"));
            }
            MonitorObservation::NetworkAvailable => {
                self.scratch_note(&format!("monitor network available at {at_ms}"));
            }
            MonitorObservation::PowerSuspending => {
                self.scratch_note(&format!("monitor power suspending at {at_ms}"));
            }
            MonitorObservation::PowerResumed => {
                self.scratch_note(&format!("monitor power resumed at {at_ms}"));
            }
        }
    }

    /// Records a dropped observer callback. Any overflow fails the capture:
    /// the verifier requires zero.
    pub fn note_observer_overflow(&mut self) {
        self.callbacks_received = self.callbacks_received.saturating_add(1);
        self.observer_overflow_count = self.observer_overflow_count.saturating_add(1);
    }

    /// Records a monitor or callback-application failure. Any failure fails
    /// the capture: the verifier requires zero of both.
    pub fn note_failure(&mut self, monitor: bool) {
        if monitor {
            self.monitor_failures = self.monitor_failures.saturating_add(1);
        } else {
            self.callback_application_failures =
                self.callback_application_failures.saturating_add(1);
        }
    }

    /// Records provider restoration during offline startup and assigns its
    /// source ordinal.
    pub fn observe_startup_restoration(
        &mut self,
        fresh_generation: u64,
        at_ms: u64,
        restored: ReadySnapshot,
    ) {
        let ordinal = self.take_ordinal();
        self.startup.restoration_observed = true;
        self.startup.restoration_ordinal = ordinal;
        self.startup.fresh_generation = fresh_generation;
        self.startup.restoration_ms = at_ms;
        self.startup.restored = Some(restored);
        if self.phase == Phase::AwaitingOfflineStart {
            self.phase = Phase::StartupRecovery;
        }
        self.scratch_note(&format!(
            "startup restoration ordinal={ordinal} generation={fresh_generation} at {at_ms}"
        ));
    }

    /// Records startup authentication and rehydration milestones.
    pub fn observe_startup_authenticated(&mut self, at_ms: u64) {
        self.startup.authenticated = true;
        self.startup.authentication_ms = at_ms;
    }

    /// Records startup rehydration and advances past offline startup.
    pub fn observe_startup_rehydrated(&mut self, at_ms: u64) {
        self.startup.rehydrated = true;
        self.startup.rehydrated_ms = at_ms;
        if self.phase == Phase::StartupRecovery {
            self.phase = Phase::AwaitingNetworkLoss;
        }
    }

    fn active_transition(&mut self) -> Option<(Phase, &mut TransitionBuilder)> {
        match self.phase {
            Phase::AwaitingNetworkLoss | Phase::NetworkRecovery => {
                Some((Phase::NetworkRecovery, &mut self.network))
            }
            Phase::AwaitingPowerLoss | Phase::PowerRecovery => {
                Some((Phase::PowerRecovery, &mut self.power))
            }
            Phase::AwaitingOfflineStart | Phase::StartupRecovery | Phase::Complete => None,
        }
    }

    /// Records provider loss for the current transition and assigns its
    /// source ordinal. Cleared state is recorded separately once retirement
    /// is confirmed, never assumed up front.
    pub fn observe_loss(&mut self, retired_generation: u64, at_ms: u64, pre: ReadySnapshot) {
        let ordinal = self.take_ordinal();
        if let Some((next, transition)) = self.active_transition() {
            transition.loss_observed = true;
            transition.loss_ordinal = ordinal;
            transition.retired_generation = retired_generation;
            transition.loss_ms = at_ms;
            transition.pre_state = Some(pre);
            self.phase = next;
            self.loss_open = true;
            self.scratch_note(&format!(
                "loss ordinal={ordinal} retired={retired_generation} at {at_ms}"
            ));
        }
    }

    /// Records retirement confirmation once the retired generation has gone
    /// silent and the fresh demand flows for the demanded entitlement. The
    /// cleared snapshot is recorded here, at the confirmation point — never
    /// assumed at loss time.
    ///
    /// There is deliberately no setter for a fence-preceding invalidation:
    /// the single upfront demand installs the generation fence before any
    /// observation begins, so an invalidation preceding it is structurally
    /// impossible; reorderings still fail through the generation-chain
    /// checks at finalize time.
    pub fn observe_retirement_confirmed(&mut self, cleared: ClearedSnapshot) {
        if let Some((_, transition)) = self.active_transition() {
            transition.environment_applied = true;
            transition.session_stopped = true;
            transition.retired_cleared = true;
            transition.cleared_state = Some(cleared);
        }
    }

    /// Records provider restoration for the current transition and assigns
    /// its source ordinal. Spurious generation advances without a preceding
    /// recorded loss are ignored (the generation still updates at the call
    /// site); only an open loss can be restored.
    pub fn observe_restoration(
        &mut self,
        fresh_generation: u64,
        at_ms: u64,
        restored: ReadySnapshot,
    ) {
        if !self.loss_open {
            return;
        }
        let ordinal = self.take_ordinal();
        if let Some((_, transition)) = self.active_transition() {
            transition.restoration_observed = true;
            transition.restoration_ordinal = ordinal;
            transition.fresh_generation = fresh_generation;
            transition.restoration_ms = at_ms;
            transition.restored = Some(restored);
            self.loss_open = false;
            self.scratch_note(&format!(
                "restoration ordinal={ordinal} fresh={fresh_generation} at {at_ms}"
            ));
        }
    }

    /// Records restoration authentication and rehydration, advancing past
    /// the current transition when both arrive.
    pub fn observe_authenticated(&mut self, at_ms: u64) {
        if let Some((_, transition)) = self.active_transition() {
            transition.authenticated = true;
            transition.authentication_ms = at_ms;
        }
    }

    /// Records rehydration and advances to the next phase.
    pub fn observe_rehydrated(&mut self, at_ms: u64) {
        let next = match self.phase {
            Phase::NetworkRecovery => Phase::AwaitingPowerLoss,
            Phase::PowerRecovery => Phase::Complete,
            other => other,
        };
        if let Some((_, transition)) = self.active_transition() {
            transition.rehydrated = true;
            transition.rehydrated_ms = at_ms;
        }
        self.phase = next;
    }

    /// Validates the complete physical sequence and assembles the report,
    /// mirroring the external verifier one rule at a time.
    ///
    /// # Errors
    ///
    /// Returns every violated rule as an actionable message.
    pub fn finalize(
        &self,
        checkpoint_unix_milliseconds: u64,
        worker_clean_stop: bool,
        readiness_qualified: bool,
    ) -> Result<CaptureReport, CaptureFailure> {
        let mut errors = Vec::new();
        self.validate_header(checkpoint_unix_milliseconds, &mut errors);
        let startup = self.assemble_startup(checkpoint_unix_milliseconds, &mut errors);
        let network = Self::assemble_transition(
            "network_offline",
            &self.network,
            checkpoint_unix_milliseconds,
            &mut errors,
        );
        let power = Self::assemble_transition(
            "suspend_resume",
            &self.power,
            checkpoint_unix_milliseconds,
            &mut errors,
        );
        Self::validate_sequence(&startup, &network, &power, &mut errors);
        if !worker_clean_stop {
            errors.push("worker did not stop cleanly".to_string());
        }
        if !readiness_qualified {
            errors.push("readiness did not qualify".to_string());
        }
        if !errors.is_empty() {
            return Err(CaptureFailure(errors));
        }
        Ok(CaptureReport {
            schema_version: REPORT_SCHEMA_VERSION,
            evidence_scope: REPORT_EVIDENCE_SCOPE.to_string(),
            platform: std::env::consts::OS.to_string(),
            completion_state: "completed".to_string(),
            source_revision: self.provenance.source_revision.clone(),
            clean_worktree: true,
            cargo_lock_path: self.provenance.cargo_lock_path.clone(),
            cargo_lock_sha256: self.provenance.cargo_lock_sha256.clone(),
            executable_path: self.provenance.executable_path.clone(),
            executable_sha256: self.provenance.executable_sha256.clone(),
            callback_source: REPORT_CALLBACK_SOURCE.to_string(),
            shipping_mode: REPORT_SHIPPING_MODE.to_string(),
            credential_source: REPORT_CREDENTIAL_SOURCE.to_string(),
            credentials_embedded: false,
            transitions_triggered_by_capture: false,
            observer_overflow: false,
            observer_overflow_count: 0,
            monitor_failures: 0,
            callback_application_failures: 0,
            callbacks_received: self.callbacks_received,
            callbacks_applied: self.callbacks_applied,
            initial_network_state: "unavailable".to_string(),
            offline_startup_observed: true,
            created_unix_milliseconds: self.created_unix_milliseconds,
            checkpoint_unix_milliseconds,
            offline_startup_recovery: startup,
            network_offline: network,
            suspend_resume: power,
            scenario_requirements_met: true,
            worker_clean_stop: true,
            finalized: true,
            readiness_qualified: true,
        })
    }

    fn validate_header(&self, checkpoint_unix_milliseconds: u64, errors: &mut Vec<String>) {
        if !self.provenance.clean_worktree {
            errors.push("capture requires a clean worktree".to_string());
        }
        if self.observer_overflow_count != 0 {
            errors.push(format!(
                "observer overflowed {} callbacks; zero are allowed",
                self.observer_overflow_count
            ));
        }
        if self.monitor_failures != 0 {
            errors.push(format!(
                "native lifecycle monitors failed {} times; zero are allowed",
                self.monitor_failures
            ));
        }
        if self.callback_application_failures != 0 {
            errors.push(format!(
                "callback application failed {} times; zero are allowed",
                self.callback_application_failures
            ));
        }
        if self.callbacks_received < 5 || self.callbacks_received != self.callbacks_applied {
            errors.push(format!(
                "every received native callback must be applied and at least five are required (received {}, applied {})",
                self.callbacks_received, self.callbacks_applied
            ));
        }
        if !self.initial_network_observed {
            errors.push(
                "capture requires an initial NativeNetworkMonitor Unavailable result".to_string(),
            );
        }
        if checkpoint_unix_milliseconds < self.created_unix_milliseconds {
            errors.push("final checkpoint predates capture creation".to_string());
        }
    }

    fn validate_sequence(
        startup: &StartupRecovery,
        network: &TransitionRecord,
        power: &TransitionRecord,
        errors: &mut Vec<String>,
    ) {
        if startup.restoration_source_ordinal >= network.loss_source_ordinal
            || network.loss_source_ordinal >= network.restoration_source_ordinal
            || network.restoration_source_ordinal >= power.loss_source_ordinal
            || power.loss_source_ordinal >= power.restoration_source_ordinal
        {
            errors.push(
                "physical callback sequence must be offline-startup restoration, separate network loss/restoration, then suspend/resume"
                    .to_string(),
            );
        }
        if startup.fresh_generation != network.retired_generation {
            errors.push(
                "separate network loss must retire the offline-startup recovery generation"
                    .to_string(),
            );
        }
        if network.fresh_generation != power.retired_generation {
            errors.push("suspend must retire the network-recovery generation".to_string());
        }
        if startup.rehydrated_unix_milliseconds > network.loss_unix_milliseconds {
            errors.push(
                "separate network loss occurred before offline-startup rehydration".to_string(),
            );
        }
        if network.rehydrated_unix_milliseconds > power.loss_unix_milliseconds {
            errors.push("suspend occurred before network-loss rehydration".to_string());
        }
    }

    fn assemble_startup(&self, checkpoint: u64, errors: &mut Vec<String>) -> StartupRecovery {
        let name = "offline_startup_recovery";
        for (property, ok) in [
            (
                "restoration_callback_observed",
                self.startup.restoration_observed,
            ),
            ("authentication_accepted", self.startup.authenticated),
            ("runtime_rehydrated", self.startup.rehydrated),
        ] {
            if !ok {
                errors.push(format!("{name}.{property} must be true"));
            }
        }
        for (property, value) in [
            (
                "restoration_source_ordinal",
                self.startup.restoration_ordinal,
            ),
            ("fresh_generation", self.startup.fresh_generation),
        ] {
            if value == 0 {
                errors.push(format!("{name}.{property} must be positive"));
            }
        }
        if !(self.startup.restoration_ms <= self.startup.authentication_ms
            && self.startup.authentication_ms <= self.startup.rehydrated_ms
            && self.startup.rehydrated_ms <= checkpoint)
        {
            errors.push(format!("{name} timestamps are not ordered"));
        }
        let restored = self.startup.restored.unwrap_or(ReadySnapshot {
            selection_installed: false,
            instrument_installed: false,
            history_request_active: true,
            live_chart_installed: false,
            pending_live_request: true,
            buffered_history_trades: 1,
            history_trade_overflow: true,
            depth_selection_installed: false,
        });
        restored.validate(&format!("{name}.restored_runtime_state"), errors);
        if self.startup.restoration_ms < self.created_unix_milliseconds {
            errors.push(format!("{name} restoration predates capture creation"));
        }
        StartupRecovery {
            restoration_callback_observed: self.startup.restoration_observed,
            authentication_accepted: self.startup.authenticated,
            runtime_rehydrated: self.startup.rehydrated,
            completed: self.startup.restoration_observed
                && self.startup.authenticated
                && self.startup.rehydrated,
            restoration_source_ordinal: self.startup.restoration_ordinal,
            fresh_generation: self.startup.fresh_generation,
            restoration_unix_milliseconds: self.startup.restoration_ms,
            authentication_unix_milliseconds: self.startup.authentication_ms,
            rehydrated_unix_milliseconds: self.startup.rehydrated_ms,
            restored_runtime_state: restored,
        }
    }

    fn check_transition_flags(
        name: &str,
        transition: &TransitionBuilder,
        errors: &mut Vec<String>,
    ) {
        for (property, ok) in [
            ("loss_callback_observed", transition.loss_observed),
            (
                "environment_apply_succeeded",
                transition.environment_applied,
            ),
            ("session_stop_confirmed", transition.session_stopped),
            ("retired_state_cleared", transition.retired_cleared),
            (
                "restoration_callback_observed",
                transition.restoration_observed,
            ),
            ("authentication_accepted", transition.authenticated),
            ("runtime_rehydrated", transition.rehydrated),
        ] {
            if !ok {
                errors.push(format!("{name}.{property} must be true"));
            }
        }
        if transition.invalidation_preceded_fence {
            errors.push(format!(
                "{name}.provider_invalidation_preceded_fence must be false"
            ));
        }
    }

    fn check_transition_ordinals(
        name: &str,
        transition: &TransitionBuilder,
        errors: &mut Vec<String>,
    ) {
        for (property, value) in [
            ("loss_source_ordinal", transition.loss_ordinal),
            ("restoration_source_ordinal", transition.restoration_ordinal),
            ("retired_generation", transition.retired_generation),
            ("fresh_generation", transition.fresh_generation),
        ] {
            if value == 0 {
                errors.push(format!("{name}.{property} must be positive"));
            }
        }
    }

    fn assemble_transition(
        name: &str,
        transition: &TransitionBuilder,
        checkpoint: u64,
        errors: &mut Vec<String>,
    ) -> TransitionRecord {
        Self::check_transition_flags(name, transition, errors);
        Self::check_transition_ordinals(name, transition, errors);
        if transition.restoration_ordinal <= transition.loss_ordinal {
            errors.push(format!(
                "{name} restoration callback must follow its loss callback"
            ));
        }
        if transition.fresh_generation <= transition.retired_generation {
            errors.push(format!(
                "{name} restoration must use a strictly newer generation"
            ));
        }
        if !(transition.loss_ms <= transition.restoration_ms
            && transition.restoration_ms <= transition.authentication_ms
            && transition.authentication_ms <= transition.rehydrated_ms
            && transition.rehydrated_ms <= checkpoint)
        {
            errors.push(format!("{name} timestamps are not ordered"));
        }
        let pre = transition.pre_state.unwrap_or(ReadySnapshot {
            selection_installed: false,
            instrument_installed: false,
            history_request_active: true,
            live_chart_installed: false,
            pending_live_request: true,
            buffered_history_trades: 1,
            history_trade_overflow: true,
            depth_selection_installed: false,
        });
        pre.validate(&format!("{name}.pre_transition_state"), errors);
        let cleared = transition.cleared_state.unwrap_or(ClearedSnapshot {
            selection_installed: true,
            instrument_installed: true,
            history_request_active: true,
            live_chart_installed: true,
            pending_live_request: true,
            buffered_history_trades: 1,
            history_trade_overflow: true,
            depth_selection_installed: true,
        });
        cleared.validate(&format!("{name}.post_retirement_state"), errors);
        let restored = transition.restored.unwrap_or(ReadySnapshot {
            selection_installed: false,
            instrument_installed: false,
            history_request_active: true,
            live_chart_installed: false,
            pending_live_request: true,
            buffered_history_trades: 1,
            history_trade_overflow: true,
            depth_selection_installed: false,
        });
        restored.validate(&format!("{name}.restored_runtime_state"), errors);
        TransitionRecord {
            loss_callback_observed: transition.loss_observed,
            environment_apply_succeeded: transition.environment_applied,
            session_stop_confirmed: transition.session_stopped,
            retired_state_cleared: transition.retired_cleared,
            restoration_callback_observed: transition.restoration_observed,
            authentication_accepted: transition.authenticated,
            runtime_rehydrated: transition.rehydrated,
            completed: transition.loss_observed
                && transition.environment_applied
                && transition.session_stopped
                && transition.retired_cleared
                && transition.restoration_observed
                && transition.authenticated
                && transition.rehydrated,
            provider_invalidation_preceded_fence: transition.invalidation_preceded_fence,
            loss_source_ordinal: transition.loss_ordinal,
            restoration_source_ordinal: transition.restoration_ordinal,
            retired_generation: transition.retired_generation,
            fresh_generation: transition.fresh_generation,
            loss_unix_milliseconds: transition.loss_ms,
            restoration_unix_milliseconds: transition.restoration_ms,
            authentication_unix_milliseconds: transition.authentication_ms,
            rehydrated_unix_milliseconds: transition.rehydrated_ms,
            pre_transition_state: pre,
            post_retirement_state: cleared,
            restored_runtime_state: restored,
        }
    }
}

/// Current time in unix milliseconds for production observation stamps.
#[must_use]
pub fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

/// Runs the `--capture-native-transitions <history-root> <report-path>
/// <cargo-lock-path> <executable-path>` command: resolves provenance from
/// the repository holding the lock file, then records one headless
/// physical-transition capture.
///
/// Paths arrive explicitly because the binary cannot reliably discover the
/// repository it was built from (a release binary may run from an install
/// version directory with no checkout nearby); the lock file lives at the
/// repository root by definition, so revision and cleanliness are read
/// there.
///
/// # Errors
///
/// Returns an actionable error for bad arguments, a dirty or unreadable
/// repository, missing inputs, or an incomplete physical sequence.
pub fn run_transition_capture_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    const USAGE: &str = "usage: axiusflow_desktop --capture-native-transitions <history-root> <report-path> <cargo-lock-path> <executable-path> [--detailed-diagnostics]";
    let history_root = arguments.next().ok_or_else(|| USAGE.to_string())?;
    let report_path = arguments.next().ok_or_else(|| USAGE.to_string())?;
    let cargo_lock = arguments.next().ok_or_else(|| USAGE.to_string())?;
    let executable = arguments.next().ok_or_else(|| USAGE.to_string())?;
    // The phase log in the history root already records every milestone;
    // the flag is accepted for tool compatibility and enables nothing more.
    if let Some(flag) = arguments.next()
        && (flag != "--detailed-diagnostics" || arguments.next().is_some())
    {
        return Err(USAGE.to_string());
    }
    let history_root = std::path::PathBuf::from(history_root);
    let report_path = std::path::PathBuf::from(report_path);
    let cargo_lock = std::path::PathBuf::from(cargo_lock);
    let executable = std::path::PathBuf::from(executable);
    for path in [&history_root, &report_path, &cargo_lock, &executable] {
        if !path.is_absolute() {
            return Err(USAGE.to_string());
        }
    }
    let repository = cargo_lock
        .parent()
        .ok_or_else(|| USAGE.to_string())?
        .to_path_buf();
    let source_revision = git_output(&repository, &["rev-parse", "HEAD"])?;
    if source_revision.len() != 40 || !source_revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("capture requires a full source revision".to_string());
    }
    let status = git_output(
        &repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.trim().is_empty() {
        return Err("capture requires a clean worktree".to_string());
    }
    run_capture(CaptureInputs {
        history_root,
        report_path,
        source_revision,
        cargo_lock: (
            cargo_lock.to_string_lossy().into_owned(),
            file_sha256_hex(&cargo_lock)?,
        ),
        executable: (
            executable.to_string_lossy().into_owned(),
            file_sha256_hex(&executable)?,
        ),
    })
}

fn git_output(repository: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|_| "capture requires a readable git repository".to_string())?;
    if !output.status.success() {
        return Err("capture requires a readable git repository".to_string());
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|_| "capture requires a readable git repository".to_string())
}

fn file_sha256_hex(path: &std::path::Path) -> Result<String, String> {
    use sha2::Digest as _;
    let bytes = std::fs::read(path).map_err(|_| "capture input file is unreadable".to_string())?;
    Ok(format!("{:x}", sha2::Sha256::digest(&bytes)))
}

// ---------------------------------------------------------------------------
// Headless capture command runner.
// ---------------------------------------------------------------------------
//
// The command observes the resident engine over authenticated IPC through
// its own isolated client (multi-client isolation holds, as proven), so no
// GPUI state is touched and no UI thread is involved. Ready states below
// describe the engine demand/generation lifecycle the recorder observes
// directly: selection/install acknowledgements, covering snapshots, live
// updates, order-book frames, and generation fencing. The operator watches
// visual recovery on their own desktop window in parallel; this command
// records the data-plane evidence.

/// Exact Rithmic contract the capture demands. An exact proven-referenceable
/// symbol keeps the evidence deterministic; the nightly soak (which resolves
/// front-month roots) covers rollover separately. Update when the venue
/// delists it: the capture fails closed naming the symbol otherwise.
const CAPTURE_SYMBOL: &str = "MNQU6";
/// Sixty-second bars: a bucket rolls visibly inside the capture window, so
/// handoff continuity is exercised by every transition.
const CAPTURE_INTERVAL_SECONDS: u32 = 60;
/// Fixed demand generation for the capture's single series demand.
const DEMAND_GENERATION: u64 = 1;
/// Overall capture deadline: three physical scenarios plus demand setup.
const CAPTURE_DEADLINE: Duration = Duration::from_mins(30);
/// Bounded observer intake per monitor; any drop fails the capture.
const OBSERVER_QUEUE: usize = 64;
/// Silence window proving the retired generation stopped producing.
const RETIRED_SILENCE: Duration = Duration::from_secs(5);

/// Outcome of one monitor observer step.
enum MonitorOutcome {
    Observation(MonitorObservation, u64),
    Failed,
}

/// Provenance inputs resolved by the command before recording starts.
pub struct CaptureInputs {
    /// Recorder scratch directory (phase log for failed-run diagnosis).
    pub history_root: std::path::PathBuf,
    /// Destination for the finalized evidence report.
    pub report_path: std::path::PathBuf,
    /// Full source revision the evidence binds to.
    pub source_revision: String,
    /// Absolute Cargo.lock path plus its SHA-256 hex digest.
    pub cargo_lock: (String, String),
    /// Absolute desktop executable path plus its SHA-256 hex digest.
    pub executable: (String, String),
}

/// Runs one headless physical-transition capture to completion.
///
/// The operator performs the physical sequence while this command records:
/// offline startup first, then a separate network loss/recovery, then
/// suspend/resume. It exits successfully only after all three recoveries
/// complete with chained generations; anything else fails closed with an
/// actionable message.
///
/// # Errors
///
/// Returns a redacted actionable error for bad inputs, a missing offline
/// start, demand failure, an incomplete physical sequence, or a deadline
/// breach. Secret material never enters errors.
pub fn run_capture(inputs: CaptureInputs) -> Result<(), String> {
    if !inputs.history_root.is_absolute() || !inputs.report_path.is_absolute() {
        return Err("capture history root and report path must be absolute".to_string());
    }
    if inputs.report_path.exists() {
        return Err("capture report path must be new".to_string());
    }
    std::fs::create_dir_all(&inputs.history_root)
        .map_err(|_| "capture history root is unavailable".to_string())?;
    if let Some(parent) = inputs.report_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| "capture report directory is unavailable".to_string())?;
    }
    let created = unix_millis_now();
    let mut recorder = TransitionRecorder::new(
        CaptureProvenance {
            source_revision: inputs.source_revision,
            clean_worktree: true,
            cargo_lock_path: inputs.cargo_lock.0,
            cargo_lock_sha256: inputs.cargo_lock.1,
            executable_path: inputs.executable.0,
            executable_sha256: inputs.executable.1,
        },
        created,
    )
    .with_scratch_directory(inputs.history_root.clone());

    let network = NativeNetworkMonitor::connect()
        .map_err(|_| "native network monitor is unavailable".to_string())?;
    if network.current() != NetworkEvent::Unavailable {
        return Err(
            "capture requires offline startup: disconnect all network access first".to_string(),
        );
    }
    recorder.apply_monitor_callback(MonitorObservation::NetworkUnavailable, created);
    let power = NativePowerMonitor::connect()
        .map_err(|_| "native power monitor is unavailable".to_string())?;

    let (monitor_tx, monitor_rx) = mpsc::sync_channel(OBSERVER_QUEUE);
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // Each observer loops for the whole capture, reporting every native
    // event through the bounded intake. A full intake drops the event but
    // counts it, so bursts can never hide; the driver fails closed on any
    // nonzero drop count at finalize time.
    let mut network_monitor = network;
    let network_handle =
        spawn_monitor(
            monitor_tx.clone(),
            dropped.clone(),
            move || match network_monitor.next_event() {
                Ok(NetworkEvent::Unavailable) => Some(MonitorOutcome::Observation(
                    MonitorObservation::NetworkUnavailable,
                    unix_millis_now(),
                )),
                Ok(NetworkEvent::Available) => Some(MonitorOutcome::Observation(
                    MonitorObservation::NetworkAvailable,
                    unix_millis_now(),
                )),
                Err(_) => None,
            },
        );
    let mut power_monitor = power;
    let power_handle = spawn_monitor(monitor_tx, dropped.clone(), move || {
        match power_monitor.next_event() {
            Ok(PowerEvent::Suspending) => Some(MonitorOutcome::Observation(
                MonitorObservation::PowerSuspending,
                unix_millis_now(),
            )),
            Ok(PowerEvent::Resumed) => Some(MonitorOutcome::Observation(
                MonitorObservation::PowerResumed,
                unix_millis_now(),
            )),
            Err(_) => None,
        }
    });
    // Handles join at finalize time for a clean worker stop.

    let mut driver = CaptureDriver::connect()?;
    let deadline = Instant::now() + CAPTURE_DEADLINE;
    let outcome = driver.observe_until_complete(&mut recorder, &monitor_rx, &dropped, deadline);
    let _ = (network_handle, power_handle);
    match outcome {
        Ok(()) => {
            let checkpoint = unix_millis_now();
            let report = recorder
                .finalize(checkpoint, true, true)
                .map_err(|failure| failure.to_string())?;
            let mut encoded = serde_json::to_vec_pretty(&report)
                .map_err(|_| "capture report could not be encoded".to_string())?;
            encoded.push(b'\n');
            std::fs::write(&inputs.report_path, encoded)
                .map_err(|_| "capture report could not be written".to_string())?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn spawn_monitor(
    sender: mpsc::SyncSender<MonitorOutcome>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut observe: impl FnMut() -> Option<MonitorOutcome> + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("axiusflow-transition-observer".to_string())
        .spawn(move || {
            loop {
                if let Some(outcome) = observe() {
                    if sender.try_send(outcome).is_err() {
                        dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                } else {
                    // The native stream ended: report the failure if the
                    // intake allows, then stop the observer.
                    if sender.try_send(MonitorOutcome::Failed).is_err() {
                        dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    break;
                }
            }
        })
        .expect("transition observer thread is unavailable")
}

/// Engine-observation driver: owns the isolated IPC client and demand, and
/// translates publications into recorder milestones.
// Boolean-heavy schema dictated by the external verifier.
#[allow(clippy::struct_excessive_bools)]
struct CaptureDriver {
    client: EngineClient,
    consumer_id: u64,
    demand_cycle: u64,
    demand_instrument_id: String,
    demand_entitlement: String,
    installed: bool,
    snapshot_seen: bool,
    live_seen: bool,
    depth_seen: bool,
    history_open: bool,
    live_open: bool,
    current_provider_generation: u64,
    retirement_pending: bool,
    retired_generation: u64,
    last_old_generation_ms: u64,
}

impl CaptureDriver {
    fn connect() -> Result<Self, String> {
        let engine = axiusflow_local_engine_client::sibling_engine_executable()?;
        let mut client = axiusflow_local_engine_client::connect_or_start_engine(&engine)?;
        let client_id = u64::from(std::process::id())
            .saturating_mul(1000)
            .saturating_add(7);
        let consumer_id = client_id.saturating_add(1);
        client.attach_client(client_id)?;
        client.register_consumer(client_id, 1, consumer_id)?;
        Ok(Self {
            client,
            consumer_id,
            demand_cycle: 0,
            demand_instrument_id: String::new(),
            demand_entitlement: String::new(),
            installed: false,
            snapshot_seen: false,
            live_seen: false,
            depth_seen: false,
            history_open: false,
            live_open: false,
            current_provider_generation: 0,
            retirement_pending: false,
            retired_generation: 0,
            last_old_generation_ms: 0,
        })
    }

    /// Demands the capture series, waiting through offline windows.
    /// Returns whether demand is fully installed. A stall is fatal only
    /// when the monitors reported online both when the attempt started and
    /// when it expired: anything else means the physical network moved
    /// mid-attempt, and the next attempt retries with a fresh generation.
    /// Every attempt and outcome is printed: the operator watches this
    /// console while performing the physical sequence.
    fn demand(
        &mut self,
        online_now: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<bool, String> {
        use std::sync::atomic::Ordering::Relaxed;
        let online_at_entry = online_now.load(Relaxed);
        eprintln!(
            "[demand] attempt {} ({} at entry)",
            self.demand_cycle.saturating_add(1),
            if online_at_entry { "online" } else { "offline" }
        );
        // Fatal only when online both at entry and at expiry: any physical
        // move mid-attempt retries instead.
        let online = || online_at_entry && online_now.load(Relaxed);
        let Some(instrument) = self.search_contract(&online)? else {
            eprintln!("[demand] search stalled while offline; waiting for reconnect");
            return Ok(false);
        };
        if !self.select_contract(&online, &instrument)? {
            eprintln!("[demand] selection stalled while offline; waiting for reconnect");
            return Ok(false);
        }
        self.client.install_provider_instrument(instrument)?;
        let series = SeriesKey {
            provider: "rithmic".to_string(),
            instrument_id: self.demand_instrument_id.clone(),
            cadence_value: CAPTURE_INTERVAL_SECONDS,
            definition_revision: 1,
            entitlement_id: self.demand_entitlement.clone(),
            cadence: SeriesCadence::FixedSeconds as i32,
        };
        self.client
            .set_series_demand(self.consumer_id, DEMAND_GENERATION, series)?;
        self.client.set_market_visibility(self.consumer_id, true)?;
        self.history_open = true;
        self.live_open = true;
        Ok(true)
    }

    fn search_contract(
        &mut self,
        online: &impl Fn() -> bool,
    ) -> Result<Option<InstallProviderInstrument>, String> {
        self.demand_cycle = self.demand_cycle.saturating_add(1);
        self.client
            .search_provider_instruments(SearchProviderInstruments {
                consumer_id: self.consumer_id,
                search_generation: self.demand_cycle,
                provider: "rithmic".to_string(),
                query: CAPTURE_SYMBOL.to_string(),
                maximum_results: 16,
            })?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if Instant::now() >= deadline {
                if online() {
                    return Err(format!(
                        "Rithmic catalog search returned no {CAPTURE_SYMBOL} contract"
                    ));
                }
                return Ok(None);
            }
            match self
                .client
                .receive_market_event_timeout(Duration::from_millis(200))
            {
                Ok(Some((_, envelope::Payload::ProviderInstrumentSearchResult(result)))) => {
                    if let Some(summary) = result
                        .instruments
                        .iter()
                        .find(|candidate| candidate.symbol == CAPTURE_SYMBOL)
                    {
                        break Ok(Some(InstallProviderInstrument {
                            provider: "rithmic".to_string(),
                            session_generation: result.provider_generation,
                            selection_generation: 1,
                            instrument_id: format!(
                                "rithmic-test:{}:{}",
                                summary.exchange, summary.symbol
                            ),
                            provider_symbol: summary.symbol.clone(),
                            display_symbol: summary.symbol.clone(),
                            venue_id: summary.exchange.clone(),
                            price_scale: 2,
                            quantity_scale: 0,
                            entitlement_id: format!(
                                "rithmic-test:{}:{}",
                                summary.exchange, summary.symbol
                            ),
                        }));
                    }
                    return Err(format!(
                        "Rithmic catalog lists no {CAPTURE_SYMBOL} contract tonight"
                    ));
                }
                Ok(Some((_, envelope::Payload::DemandError(error)))) => {
                    return Err(format!("Rithmic catalog demand failed: {}", error.detail));
                }
                Ok(_) | Err(_) => {}
            }
        }
    }

    fn select_contract(
        &mut self,
        online: &impl Fn() -> bool,
        instrument: &InstallProviderInstrument,
    ) -> Result<bool, String> {
        self.client.select_provider_instrument(
            axiusflow_engine_protocol::SelectProviderInstrument {
                consumer_id: self.consumer_id,
                selection_generation: self.demand_cycle,
                search_generation: self.demand_cycle,
                provider: "rithmic".to_string(),
                symbol: instrument.provider_symbol.clone(),
                exchange: instrument.venue_id.clone(),
                entitlement_id: instrument.entitlement_id.clone(),
            },
        )?;
        self.demand_instrument_id
            .clone_from(&instrument.instrument_id);
        self.demand_entitlement
            .clone_from(&instrument.entitlement_id);
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if Instant::now() >= deadline {
                if online() {
                    return Err(format!("Rithmic selection never resolved {CAPTURE_SYMBOL}"));
                }
                return Ok(false);
            }
            match self
                .client
                .receive_market_event_timeout(Duration::from_millis(200))
            {
                Ok(Some((_, envelope::Payload::ProviderInstrumentSelection(selection)))) => {
                    if selection.instrument.is_some() {
                        return Ok(true);
                    }
                }
                Ok(Some((_, envelope::Payload::DemandError(error)))) => {
                    return Err(format!("Rithmic selection failed: {}", error.detail));
                }
                Ok(_) | Err(_) => {}
            }
        }
    }

    fn ready_snapshot(&self) -> ReadySnapshot {
        ReadySnapshot {
            selection_installed: self.installed,
            instrument_installed: self.installed,
            history_request_active: self.history_open,
            live_chart_installed: self.live_seen,
            pending_live_request: self.live_open,
            depth_selection_installed: self.depth_seen,
            ..ReadySnapshot::installed()
        }
    }

    fn observe_until_complete(
        &mut self,
        recorder: &mut TransitionRecorder,
        monitor_rx: &mpsc::Receiver<MonitorOutcome>,
        dropped: &std::sync::Arc<std::sync::atomic::AtomicU64>,
        deadline: Instant,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut demanded = false;
        let online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        loop {
            if Instant::now() >= deadline {
                return Err(
                    "transition capture timed out waiting for the physical sequence".to_string(),
                );
            }
            while let Ok(outcome) = monitor_rx.try_recv() {
                match outcome {
                    MonitorOutcome::Observation(observation, at_ms) => {
                        match observation {
                            MonitorObservation::NetworkUnavailable => {
                                online.store(false, Relaxed);
                            }
                            MonitorObservation::NetworkAvailable => {
                                online.store(true, Relaxed);
                            }
                            MonitorObservation::PowerSuspending
                            | MonitorObservation::PowerResumed => {}
                        }
                        recorder.apply_monitor_callback(observation, at_ms);
                    }
                    MonitorOutcome::Failed => recorder.note_failure(true),
                }
            }
            for _ in 0..dropped.swap(0, std::sync::atomic::Ordering::Relaxed) {
                recorder.note_observer_overflow();
            }
            if !demanded {
                // Demand retries across the offline window with fresh
                // generations; only a stall online at both entry and expiry
                // is fatal.
                demanded = self.demand(&online)?;
                continue;
            }
            if let Ok(Some((_, payload))) = self
                .client
                .receive_market_event_timeout(Duration::from_millis(200))
            {
                self.apply_publication(recorder, payload)?;
            }
            if self.snapshot_seen {
                self.history_open = false;
            }
            self.confirm_retirement(recorder);
            if matches!(recorder.phase, Phase::Complete) {
                return Ok(());
            }
        }
    }

    /// Confirms retirement once fresh demand flows while the retired
    /// generation stays silent. The cleared state is recorded here, at the
    /// confirmation point — never assumed at loss time.
    fn confirm_retirement(&mut self, recorder: &mut TransitionRecorder) {
        if !self.retirement_pending || !self.live_seen || !self.installed {
            return;
        }
        if self.current_provider_generation <= self.retired_generation {
            return;
        }
        let silent_ms = unix_millis_now().saturating_sub(self.last_old_generation_ms);
        if silent_ms < u64::try_from(RETIRED_SILENCE.as_millis()).unwrap_or(u64::MAX) {
            return;
        }
        self.retirement_pending = false;
        recorder.observe_retirement_confirmed(ClearedSnapshot::cleared());
    }

    fn apply_publication(
        &mut self,
        recorder: &mut TransitionRecorder,
        payload: envelope::Payload,
    ) -> Result<(), String> {
        match payload {
            envelope::Payload::ProviderInstrumentInstalled(_) => {
                self.installed = true;
            }
            envelope::Payload::SeriesSnapshot(snapshot) => {
                self.snapshot_seen = true;
                self.history_open = false;
                let restored = self.note_generation(recorder, snapshot.provider_generation);
                if restored && self.snapshot_completes_restoration(recorder) {
                    self.complete_restoration(recorder);
                }
            }
            envelope::Payload::SeriesUpdate(update) => {
                self.live_seen = true;
                self.live_open = false;
                let restored = self.note_generation(recorder, update.provider_generation);
                if restored && self.snapshot_completes_restoration(recorder) {
                    self.complete_restoration(recorder);
                }
            }
            envelope::Payload::OrderBookSnapshot(_) => {
                self.depth_seen = true;
            }
            envelope::Payload::DemandError(_) => {
                // A terminal demand error before any chart data ever flowed
                // fails fast: the feed cannot serve this contract tonight, so
                // waiting out the deadline would prove nothing. Details stay
                // engine-side; only the fact travels further.
                if self.current_provider_generation == 0 {
                    return Err("Rithmic demand failed before first chart data".to_string());
                }
                self.record_loss(recorder);
            }
            envelope::Payload::Fault(_) => {
                if self.current_provider_generation == 0 {
                    return Err("Rithmic demand faulted before first chart data".to_string());
                }
                self.record_loss(recorder);
            }
            _ => {}
        }
        Ok(())
    }

    fn note_generation(&mut self, recorder: &mut TransitionRecorder, generation: u64) -> bool {
        if generation == 0 {
            return false;
        }
        if generation > self.current_provider_generation {
            // A newer provider generation flowing after a loss is the
            // restoration the recorder sequences. Spurious advances without
            // a preceding recorded loss update the watermark only: only an
            // open loss can be restored, so stray bumps can never fabricate
            // a recovery.
            let fresh = generation;
            self.current_provider_generation = generation;
            if recorder.loss_open {
                let at_ms = unix_millis_now();
                let restored = self.ready_snapshot();
                recorder.observe_restoration(fresh, at_ms, restored);
                recorder.observe_authenticated(at_ms);
                return true;
            }
        } else if self.retirement_pending && generation == self.retired_generation {
            self.last_old_generation_ms = unix_millis_now();
        }
        false
    }

    fn snapshot_completes_restoration(&self, recorder: &TransitionRecorder) -> bool {
        // Restoration completes when chart data flows again with the ready
        // state installed; the recorder's phase tells which recovery this is.
        !matches!(
            recorder.phase,
            Phase::AwaitingOfflineStart | Phase::Complete
        ) && self.installed
            && self.live_seen
    }

    fn complete_restoration(&mut self, recorder: &mut TransitionRecorder) {
        // Called only immediately after note_generation recorded a fresh
        // restoration, so rehydration strictly follows it. The startup
        // branch carries its own trio; later recoveries already recorded
        // authentication alongside restoration.
        let at_ms = unix_millis_now();
        let restored = self.ready_snapshot();
        match recorder.phase {
            Phase::AwaitingOfflineStart | Phase::StartupRecovery => {
                if !recorder.startup.restoration_observed {
                    recorder.observe_startup_restoration(
                        self.current_provider_generation,
                        at_ms,
                        restored,
                    );
                    recorder.observe_startup_authenticated(at_ms);
                    recorder.observe_startup_rehydrated(at_ms);
                }
            }
            _ => {
                recorder.observe_rehydrated(at_ms);
            }
        }
    }

    fn record_loss(&mut self, recorder: &mut TransitionRecorder) {
        // Terminal demand errors and faults retire the current provider
        // generation. Retirement (silence-confirmed) and the cleared state
        // are recorded separately; monitor-callback correlation across
        // offline versus suspend windows is validated at finalize time, so
        // misattributed phases fail closed there.
        if self.current_provider_generation == 0 {
            return;
        }
        let at_ms = unix_millis_now();
        let pre = self.ready_snapshot();
        recorder.observe_loss(self.current_provider_generation, at_ms, pre);
        self.retirement_pending = true;
        self.retired_generation = self.current_provider_generation;
        self.last_old_generation_ms = at_ms;
        self.installed = false;
        self.live_seen = false;
        self.depth_seen = false;
        self.history_open = true;
        self.live_open = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> CaptureProvenance {
        CaptureProvenance {
            source_revision: "a".repeat(40),
            clean_worktree: true,
            cargo_lock_path: "Cargo.lock".to_string(),
            cargo_lock_sha256: "b".repeat(64),
            executable_path: "axiusflow_desktop.exe".to_string(),
            executable_sha256: "c".repeat(64),
        }
    }

    fn ready() -> ReadySnapshot {
        ReadySnapshot::installed()
    }

    fn cleared() -> ClearedSnapshot {
        ClearedSnapshot::cleared()
    }

    fn callbacks(recorder: &mut TransitionRecorder) {
        for _ in 0..5 {
            recorder.apply_monitor_callback(MonitorObservation::NetworkUnavailable, 1000);
        }
    }

    fn full_sequence() -> TransitionRecorder {
        let mut recorder = TransitionRecorder::new(provenance(), 1000);
        recorder.apply_monitor_callback(MonitorObservation::NetworkUnavailable, 1000);
        recorder.observe_startup_restoration(10, 1100, ready());
        recorder.observe_startup_authenticated(1200);
        recorder.observe_startup_rehydrated(1300);
        recorder.observe_loss(10, 1400, ready());
        recorder.observe_retirement_confirmed(cleared());
        recorder.observe_restoration(11, 1500, ready());
        recorder.observe_authenticated(1600);
        recorder.observe_rehydrated(1700);
        recorder.observe_loss(11, 1800, ready());
        recorder.observe_retirement_confirmed(cleared());
        recorder.observe_restoration(12, 1900, ready());
        recorder.observe_authenticated(2000);
        recorder.observe_rehydrated(2100);
        recorder
    }

    #[test]
    fn complete_physical_sequence_finalizes() {
        let mut recorder = full_sequence();
        callbacks(&mut recorder);
        let report = recorder
            .finalize(2200, true, true)
            .expect("complete capture");
        assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);
        assert_eq!(
            report.offline_startup_recovery.restoration_source_ordinal,
            1
        );
        assert_eq!(report.network_offline.loss_source_ordinal, 2);
        assert_eq!(report.network_offline.restoration_source_ordinal, 3);
        assert_eq!(report.suspend_resume.loss_source_ordinal, 4);
        assert_eq!(report.suspend_resume.restoration_source_ordinal, 5);
        assert_eq!(report.callbacks_received, 6);
        assert_eq!(report.callbacks_applied, 6);
        let encoded = serde_json::to_string(&report).expect("report encodes");
        let decoded: serde_json::Value = serde_json::from_str(&encoded).expect("report decodes");
        assert_eq!(decoded["evidence_scope"], REPORT_EVIDENCE_SCOPE);
        assert_eq!(decoded["network_offline"]["fresh_generation"], 11);
    }

    #[test]
    fn skipped_suspend_phase_fails_closed() {
        let mut recorder = TransitionRecorder::new(provenance(), 1000);
        recorder.apply_monitor_callback(MonitorObservation::NetworkUnavailable, 1000);
        recorder.observe_startup_restoration(10, 1100, ready());
        recorder.observe_startup_authenticated(1200);
        recorder.observe_startup_rehydrated(1300);
        recorder.observe_loss(10, 1400, ready());
        recorder.observe_retirement_confirmed(cleared());
        recorder.observe_restoration(11, 1500, ready());
        recorder.observe_authenticated(1600);
        recorder.observe_rehydrated(1700);
        callbacks(&mut recorder);
        let failure = recorder
            .finalize(1800, true, true)
            .expect_err("suspend missing");
        assert!(
            failure
                .0
                .iter()
                .any(|message| message.contains("suspend_resume.loss_callback_observed")),
            "unexpected failures: {:?}",
            failure.0
        );
    }

    #[test]
    fn unordered_timestamps_fail_closed() {
        let mut recorder = TransitionRecorder::new(provenance(), 1000);
        recorder.apply_monitor_callback(MonitorObservation::NetworkUnavailable, 1000);
        recorder.observe_startup_restoration(10, 1300, ready());
        recorder.observe_startup_authenticated(1200);
        recorder.observe_startup_rehydrated(1250);
        callbacks(&mut recorder);
        let failure = recorder
            .finalize(2000, true, true)
            .expect_err("timestamps unordered");
        assert!(
            failure
                .0
                .iter()
                .any(|message| message.contains("not ordered")),
            "unexpected failures: {:?}",
            failure.0
        );
    }

    #[test]
    fn broken_generation_chain_fails_closed() {
        let mut recorder = full_sequence();
        callbacks(&mut recorder);
        recorder.network.fresh_generation = 10;
        let failure = recorder
            .finalize(2200, true, true)
            .expect_err("chain broken");
        assert!(
            failure
                .0
                .iter()
                .any(|message| message.contains("strictly newer generation")),
            "unexpected failures: {:?}",
            failure.0
        );
    }

    #[test]
    fn observer_overflow_fails_closed() {
        let mut recorder = full_sequence();
        callbacks(&mut recorder);
        recorder.note_observer_overflow();
        let failure = recorder.finalize(2200, true, true).expect_err("overflow");
        assert!(
            failure.0.iter().any(|message| message.contains("overflow")),
            "unexpected failures: {:?}",
            failure.0
        );
    }

    #[test]
    fn started_online_fails_closed() {
        let recorder = TransitionRecorder::new(provenance(), 1000);
        let failure = recorder
            .finalize(2000, true, true)
            .expect_err("never offline");
        assert!(
            failure
                .0
                .iter()
                .any(|message| message.contains("initial NativeNetworkMonitor")),
            "unexpected failures: {:?}",
            failure.0
        );
    }
}
