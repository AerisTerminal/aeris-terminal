//! Fail-closed authorization-service boundary.
//!
//! Verifies Ed25519 service-access JWTs against a pinned JWKS snapshot through
//! `crates/security`, evaluates exact policy grants through the authorization
//! domain, and serves a bounded typed HTTP boundary. Policy distribution is a
//! monotonically increasing versioned snapshot swap: removing a grant in a new
//! revision revokes it immediately. Every failure — missing configuration,
//! expired or foreign token, unknown grant, malformed request — fails closed.

mod boundary;
mod policy_source;

use axiusflow_security::{Ed25519JwtVerifier, Ed25519KeySetSnapshot, SystemVerificationClock};
use std::{env, fs, process::ExitCode};

const CLOCK_SKEW_SECONDS: u64 = 60;

fn main() -> ExitCode {
    match bootstrap() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("axiusflow_authorization_service fail-closed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn bootstrap() -> Result<ExitCode, String> {
    let mut listen = None;
    let mut jwks_path = None;
    let mut policy_path = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--listen" => {
                listen = Some(arguments.next().ok_or("--listen requires an address")?);
            }
            "--jwks" => {
                jwks_path = Some(arguments.next().ok_or("--jwks requires a path")?);
            }
            "--policy" => {
                policy_path = Some(arguments.next().ok_or("--policy requires a path")?);
            }
            other => return Err(format!("unsupported argument: {other}")),
        }
    }
    let listen = listen.ok_or("--listen is required")?;
    let jwks_path = jwks_path.ok_or("--jwks is required")?;
    let policy_path = policy_path.ok_or("--policy is required")?;

    let jwks_json = fs::read_to_string(&jwks_path)
        .map_err(|error| format!("cannot read JWKS snapshot {jwks_path}: {error}"))?;
    let key_set = Ed25519KeySetSnapshot::try_from_jwks_json(1, &jwks_json)
        .map_err(|error| format!("invalid JWKS snapshot: {error}"))?;
    let verifier =
        Ed25519JwtVerifier::try_new(key_set, SystemVerificationClock, CLOCK_SKEW_SECONDS)
            .map_err(|error| format!("verifier configuration failed: {error}"))?;

    let policy = policy_source::load_policy(&policy_path)?;
    let service = boundary::AuthorizationBoundary::new(verifier, policy);
    println!(
        "axiusflow_authorization_service listener_started=true policy_version={} key_revision={}",
        service.policy_version(),
        service.key_revision()
    );
    service
        .serve(&listen)
        .map_err(|error| format!("listener failed: {error}"))?;
    Ok(ExitCode::SUCCESS)
}
