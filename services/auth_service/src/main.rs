//! First-party Rust authentication service.
//!
//! Owns credentials, authentication sessions, and Ed25519 token issuance per
//! Section 3.2: password verification is memory-hard `argon2`, state is
//! authoritative in `PostgreSQL`, tokens are short-lived `EdDSA` JWTs that
//! `crates/security` verifies offline against the revisioned JWKS this service
//! publishes. Application grants and entitlements stay with the authorization
//! domain; this service never evaluates them.

mod boundary;
mod credentials;
mod issuance;

use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match bootstrap() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("axiusflow_auth_service fail-closed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn bootstrap() -> Result<ExitCode, String> {
    let mut listen = None;
    let mut signing_key_path = None;
    let mut database_url = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--listen" => {
                listen = Some(arguments.next().ok_or("--listen requires an address")?);
            }
            "--signing-key" => {
                signing_key_path = Some(arguments.next().ok_or("--signing-key requires a path")?);
            }
            "--database" => {
                database_url = Some(
                    arguments
                        .next()
                        .ok_or("--database requires connection parts")?,
                );
            }
            other => return Err(format!("unsupported argument: {other}")),
        }
    }
    let listen = listen.ok_or("--listen is required")?;
    let signing_key_path = signing_key_path.ok_or("--signing-key is required")?;
    let database_url = database_url.ok_or("--database is required")?;

    let service = boundary::AuthBoundary::bootstrap(&signing_key_path, &database_url)?;
    println!(
        "axiusflow_auth_service listener_started=true key_revision={}",
        service.key_revision()
    );
    service
        .serve(&listen)
        .map_err(|error| format!("listener failed: {error}"))?;
    Ok(ExitCode::SUCCESS)
}
