//! The candidate readiness report: `aeris_desktop --desktop-readiness <path>` writes it
//! and the launcher accepts it before a release activates. Both sides use this one type,
//! so its JSON shape is the contract between a stable launcher and a newer candidate.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The readiness report wire schema the launcher accepts.
pub const READINESS_REPORT_SCHEMA_VERSION: u32 = 6;

/// The vault namespace a production data root reports.
pub const PRODUCTION_VAULT_NAMESPACE: &str = "production";

/// The vault namespace a validation data root reports.
pub const VALIDATION_VAULT_NAMESPACE: &str = "validation";

/// What a candidate desktop observed after starting its services against the data root.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessReport {
    pub schema_version: u32,
    pub release_identity: String,
    pub install_generation: u64,
    pub desktop_process_id: u32,
    pub workspace_revision: u64,
    /// Market providers registered with the candidate's market runtime. Registration
    /// needs no sign-in, provider login or network, so a fresh install reports every
    /// built-in provider; zero means the candidate was built without its providers.
    pub provider_count: usize,
    pub workspace_restored: bool,
    pub validation_mode: bool,
    pub instance_lock: bool,
    pub log_path: String,
    pub vault_namespace: String,
    pub services: ServiceReadiness,
}

/// Whether each in-process owner started.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceReadiness {
    pub market: ServiceStarted,
    pub account: ServiceStarted,
    pub trading: ServiceStarted,
    pub context: ServiceStarted,
}

/// Whether one owner started; encoded as a plain JSON boolean.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ServiceStarted(pub bool);

/// Why the launcher refuses a candidate's readiness report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessRejection {
    SchemaVersion,
    ReleaseIdentity,
    InstallGeneration,
    ProcessId,
    NoProviders,
    WorkspaceNotRestored,
    ValidationRoot,
    InstanceLockNotHeld,
    LogPathMissing,
    VaultNamespace,
    ServiceNotReady,
}

impl fmt::Display for ReadinessRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SchemaVersion => "unsupported report schema",
            Self::ReleaseIdentity => "report is for another release",
            Self::InstallGeneration => "report is for another install generation",
            Self::ProcessId => "report has no desktop process",
            Self::NoProviders => "no market provider is registered",
            Self::WorkspaceNotRestored => "workspace was not restored",
            Self::ValidationRoot => "report came from a validation data root",
            Self::InstanceLockNotHeld => "instance lock was not held",
            Self::LogPathMissing => "report has no log path",
            Self::VaultNamespace => "report used a non-production vault namespace",
            Self::ServiceNotReady => "a desktop service did not start",
        })
    }
}

impl ReadinessReport {
    /// Accepts the report only when it comes from the candidate `release_identity` and
    /// `install_generation`, ran on the production data root under its instance lock, and
    /// every service started.
    ///
    /// # Errors
    /// Returns the first check the report fails.
    pub fn accept_candidate(
        &self,
        release_identity: &str,
        install_generation: u64,
    ) -> Result<(), ReadinessRejection> {
        let services = self.services;
        let checks = [
            (
                self.schema_version == READINESS_REPORT_SCHEMA_VERSION,
                ReadinessRejection::SchemaVersion,
            ),
            (
                self.release_identity == release_identity,
                ReadinessRejection::ReleaseIdentity,
            ),
            (
                self.install_generation == install_generation,
                ReadinessRejection::InstallGeneration,
            ),
            (self.desktop_process_id != 0, ReadinessRejection::ProcessId),
            (self.provider_count > 0, ReadinessRejection::NoProviders),
            (
                self.workspace_restored,
                ReadinessRejection::WorkspaceNotRestored,
            ),
            (!self.validation_mode, ReadinessRejection::ValidationRoot),
            (self.instance_lock, ReadinessRejection::InstanceLockNotHeld),
            (
                !self.log_path.is_empty(),
                ReadinessRejection::LogPathMissing,
            ),
            (
                self.vault_namespace == PRODUCTION_VAULT_NAMESPACE,
                ReadinessRejection::VaultNamespace,
            ),
            (
                [
                    services.market,
                    services.account,
                    services.trading,
                    services.context,
                ]
                .iter()
                .all(|started| started.0),
                ReadinessRejection::ServiceNotReady,
            ),
        ];
        checks
            .into_iter()
            .find_map(|(passed, rejection)| (!passed).then_some(rejection))
            .map_or(Ok(()), Err)
    }

    /// Encodes the report as one JSON line.
    ///
    /// # Errors
    /// Returns an error when serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut encoded = serde_json::to_vec(self)?;
        encoded.push(b'\n');
        Ok(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_report() -> ReadinessReport {
        ReadinessReport {
            schema_version: READINESS_REPORT_SCHEMA_VERSION,
            release_identity: "aeris-1.2.3".to_string(),
            install_generation: 7,
            desktop_process_id: 4_242,
            workspace_revision: 9,
            provider_count: 2,
            workspace_restored: true,
            validation_mode: false,
            instance_lock: true,
            log_path: "C:\\Aeris\\logs\\aeris.log".to_string(),
            vault_namespace: PRODUCTION_VAULT_NAMESPACE.to_string(),
            services: ServiceReadiness {
                market: ServiceStarted(true),
                account: ServiceStarted(true),
                trading: ServiceStarted(true),
                context: ServiceStarted(true),
            },
        }
    }

    #[test]
    fn encoded_report_round_trips_and_is_accepted() {
        let report = candidate_report();
        let encoded = report.encode().expect("report encodes");
        assert_eq!(encoded.last(), Some(&b'\n'));
        let decoded: ReadinessReport = serde_json::from_slice(&encoded).expect("report decodes");
        assert_eq!(decoded, report);
        assert_eq!(decoded.accept_candidate("aeris-1.2.3", 7), Ok(()));
    }

    #[test]
    fn wire_shape_is_the_schema_six_contract() {
        let wire = r#"{"schema_version":6,"release_identity":"aeris-1.2.3","install_generation":7,"desktop_process_id":4242,"workspace_revision":9,"provider_count":2,"workspace_restored":true,"validation_mode":false,"instance_lock":true,"log_path":"C:\\Aeris\\logs\\aeris.log","vault_namespace":"production","services":{"market":true,"account":true,"trading":true,"context":true}}"#;
        let decoded: ReadinessReport = serde_json::from_str(wire).expect("schema 6 decodes");
        assert_eq!(decoded, candidate_report());
        assert_eq!(
            serde_json::to_string(&decoded).expect("report encodes"),
            wire
        );
    }

    #[test]
    fn unknown_or_missing_fields_are_rejected() {
        let mut value = serde_json::to_value(candidate_report()).expect("report value");
        value["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<ReadinessReport>(value.clone()).is_err());
        let object = value.as_object_mut().expect("object");
        object.remove("unexpected");
        object.remove("provider_count");
        assert!(serde_json::from_value::<ReadinessReport>(value).is_err());
        let mut services = serde_json::to_value(candidate_report()).expect("report value");
        services["services"]["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<ReadinessReport>(services).is_err());
    }

    #[test]
    fn each_failed_check_names_its_rejection() {
        type BreakReport = fn(&mut ReadinessReport);
        let cases: [(BreakReport, ReadinessRejection); 11] = [
            (|r| r.schema_version = 5, ReadinessRejection::SchemaVersion),
            (
                |r| r.release_identity = "aeris-1.2.2".to_string(),
                ReadinessRejection::ReleaseIdentity,
            ),
            (
                |r| r.install_generation = 6,
                ReadinessRejection::InstallGeneration,
            ),
            (|r| r.desktop_process_id = 0, ReadinessRejection::ProcessId),
            (|r| r.provider_count = 0, ReadinessRejection::NoProviders),
            (
                |r| r.workspace_restored = false,
                ReadinessRejection::WorkspaceNotRestored,
            ),
            (
                |r| r.validation_mode = true,
                ReadinessRejection::ValidationRoot,
            ),
            (
                |r| r.instance_lock = false,
                ReadinessRejection::InstanceLockNotHeld,
            ),
            (|r| r.log_path.clear(), ReadinessRejection::LogPathMissing),
            (
                |r| r.vault_namespace = VALIDATION_VAULT_NAMESPACE.to_string(),
                ReadinessRejection::VaultNamespace,
            ),
            (
                |r| r.services.context = ServiceStarted(false),
                ReadinessRejection::ServiceNotReady,
            ),
        ];
        for (break_report, rejection) in cases {
            let mut report = candidate_report();
            break_report(&mut report);
            assert_eq!(report.accept_candidate("aeris-1.2.3", 7), Err(rejection));
        }
    }
}
