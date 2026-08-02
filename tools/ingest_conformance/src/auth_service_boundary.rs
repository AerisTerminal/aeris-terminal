//! Running authentication-service conformance.
//!
//! Spawns the real `auth_service` against a lane `PostgreSQL` and proves signup,
//! argon2 password verification with uniform failure behavior, Ed25519 token
//! issuance, and the decisive Section 3.2 check: issued tokens verify offline
//! through `crates/security` against the service's own published JWKS. Plaintext
//! loopback proves the service core only; OAuth/OIDC, PKCE, second factors, TLS,
//! and key-management deployment are not exercised and not claimed.

use axiusflow_security::{
    Ed25519JwtVerifier, Ed25519KeySetSnapshot, JwtVerificationRequest, JwtVerifier,
    SystemVerificationClock, TokenPurpose,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    env,
    error::Error,
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_2_auth_service";

#[derive(Serialize)]
struct AuthBehaviorEvidence {
    startup_and_migrations: &'static str,
    signup: &'static str,
    duplicate_signup_rejected: &'static str,
    sign_in: &'static str,
    wrong_password_rejected: &'static str,
    unknown_principal_rejected: &'static str,
    token_verifies_through_crates_security: &'static str,
    jwks_revisioned: &'static str,
    malformed_rejected: &'static str,
    oversized_rejected: &'static str,
    oauth_oidc: &'static str,
    pkce_flow: &'static str,
    second_factor: &'static str,
    tls: &'static str,
}

#[derive(Serialize)]
struct AuthServiceReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    behavior: AuthBehaviorEvidence,
    limitations: [&'static str; 4],
}

/// Runs the authentication-service conformance and writes one evidence artifact.
pub fn run(
    service_binary: &Path,
    pg_host: &str,
    pg_port: u16,
    pg_user: &str,
    pg_password: &str,
    pg_database: &str,
    report_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for auth service evidence")?;
    let directory = env::temp_dir().join(format!("axiusflow-auth-lane-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    let key_path = directory.join("signing_key");
    let mut key_bytes = [0_u8; 32];
    getrandom::fill(&mut key_bytes)?;
    fs::write(&key_path, key_bytes)?;

    let port = 20_000 + (std::process::id() % 1_000) as u16;
    let address = format!("127.0.0.1:{port}");
    let database_url = format!(
        "host={pg_host} port={pg_port} user={pg_user} password={pg_password} dbname={pg_database}"
    );
    let mut service = Command::new(service_binary)
        .arg("--listen")
        .arg(&address)
        .arg("--signing-key")
        .arg(&key_path)
        .arg("--database")
        .arg(&database_url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = exercise(&address);
    let _ = service.kill();
    let output = service.wait_with_output()?;
    let _ = fs::remove_dir_all(&directory);
    if result.is_err() {
        eprintln!(
            "auth service stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    result?;

    let report = AuthServiceReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        behavior: AuthBehaviorEvidence {
            startup_and_migrations: "passed",
            signup: "passed",
            duplicate_signup_rejected: "passed",
            sign_in: "passed",
            wrong_password_rejected: "passed",
            unknown_principal_rejected: "passed",
            token_verifies_through_crates_security: "passed",
            jwks_revisioned: "passed",
            malformed_rejected: "passed",
            oversized_rejected: "passed",
            oauth_oidc: "not_exercised",
            pkce_flow: "not_exercised",
            second_factor: "not_exercised",
            tls: "not_exercised",
        },
        limitations: [
            "plaintext_loopback",
            "no_oauth_pkce_second_factor",
            "file_provisioned_signing_key",
            "no_tls",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "auth_service=passed signup=true sign_in=true wrong_password_rejected=true unknown_rejected=true token_verifies_through_crates_security=true jwks=true report={}",
        report_path.display()
    );
    Ok(())
}

fn exercise(address: &str) -> Result<(), Box<dyn Error>> {
    wait_for_health(address)?;

    let (status, _) = post_json(
        address,
        "/signup",
        &json!({"principal_id": "user-1", "password": "correct horse battery staple"}),
    )?;
    require(status == 201, "signup failed")?;

    let (status, _) = post_json(
        address,
        "/signup",
        &json!({"principal_id": "user-1", "password": "correct horse battery staple"}),
    )?;
    require(status == 400, "duplicate signup was not rejected")?;

    let (status, _) = post_json(
        address,
        "/sign_in",
        &json!({"principal_id": "user-1", "password": "wrong password entirely"}),
    )?;
    require(status == 401, "wrong password was not rejected")?;

    let (status, _) = post_json(
        address,
        "/sign_in",
        &json!({"principal_id": "ghost-9", "password": "correct horse battery staple"}),
    )?;
    require(status == 401, "unknown principal was not rejected")?;

    let (status, body) = post_json(
        address,
        "/sign_in",
        &json!({"principal_id": "user-1", "password": "correct horse battery staple", "device_id": "lane-device-1"}),
    )?;
    require(status == 200, "sign-in failed")?;
    let response: Value = serde_json::from_str(&body)?;
    let token = response["token"]
        .as_str()
        .ok_or("token response is incomplete")?;

    let (status, jwks) = get(address, "/jwks")?;
    require(status == 200, "JWKS publication failed")?;
    let key_set = Ed25519KeySetSnapshot::try_from_jwks_json(1, &jwks)
        .map_err(|error| format!("service JWKS was rejected by crates/security: {error}"))?;
    let verifier = Ed25519JwtVerifier::try_new(key_set, SystemVerificationClock, 60)
        .map_err(|error| format!("verifier configuration failed: {error}"))?;
    let identity = verifier
        .verify(JwtVerificationRequest {
            encoded_token: token,
            expected_issuer: "axiusflow",
            expected_audience: "authorization_service",
            purpose: TokenPurpose::ServiceAccess,
        })
        .map_err(|error| format!("service token failed crates/security verification: {error}"))?;
    require(
        identity.subject_id == "user-1"
            && identity.session_id.as_deref() == response["session_id"].as_str(),
        "verified identity does not match the sign-in",
    )?;

    let (status, _) = post_raw(address, "/sign_in", b"not json")?;
    require(status == 400, "malformed body was not rejected")?;
    let oversized = vec![b'x'; 8_192];
    let (status, _) = post_raw(address, "/sign_in", &oversized)?;
    require(status == 413, "oversized body was not rejected")?;
    Ok(())
}

fn wait_for_health(address: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok((200, _)) = get(address, "/health") {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("auth service did not become healthy".into())
}

fn get(address: &str, path: &str) -> Result<(u16, String), Box<dyn Error>> {
    request(address, "GET", path, &[])
}

fn post_json(address: &str, path: &str, value: &Value) -> Result<(u16, String), Box<dyn Error>> {
    post_raw(address, path, value.to_string().as_bytes())
}

fn post_raw(address: &str, path: &str, body: &[u8]) -> Result<(u16, String), Box<dyn Error>> {
    request(address, "POST", path, body)
}

fn request(
    address: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<(u16, String), Box<dyn Error>> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or("malformed response")?;
    let body_start = text.find("\r\n\r\n").map_or(text.len(), |index| index + 4);
    Ok((status, text[body_start..].to_string()))
}

fn require(condition: bool, message: &str) -> Result<(), Box<dyn Error>> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
