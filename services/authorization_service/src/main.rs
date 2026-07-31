//! Fail-closed authorization-service foundation.
//!
//! A network listener is intentionally not started until the gRPC stack and
//! workload identity dependencies are pinned.

use axiusflow_authorization::AuthorizationDecision;
use std::process::ExitCode;

fn main() -> ExitCode {
    let startup_decision = AuthorizationDecision::policy_unavailable();
    eprintln!(
        "axiusflow_authorization_service inactive: policy_loaded={} listener_started=false default_allowed={}",
        startup_decision.policy_version().is_some(),
        startup_decision.is_allowed()
    );
    ExitCode::FAILURE
}
