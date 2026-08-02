//! Running authorization-service boundary conformance.
//!
//! Spawns the real service against a lane-generated JWKS and policy snapshot and
//! proves startup, Ed25519 service-access token verification, allow and deny
//! decisions, expiry and foreign-key rejection, monotonic policy replacement
//! (revocation), and bounded failure behavior. Plaintext loopback proves the
//! boundary only; TLS, workload identity, sessions, and gRPC are not exercised
//! and not claimed.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    env,
    error::Error,
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_2_authorization_boundary";
const KEY_ID: &str = "lane-key-1";

#[derive(Serialize)]
struct BoundaryBehaviorEvidence {
    startup: &'static str,
    token_verification: &'static str,
    allow_decision: &'static str,
    deny_decision: &'static str,
    expired_token_rejected: &'static str,
    foreign_key_rejected: &'static str,
    monotonic_policy_swap: &'static str,
    revocation: &'static str,
    malformed_rejected: &'static str,
    oversized_rejected: &'static str,
    tls: &'static str,
    sessions: &'static str,
    grpc_transport: &'static str,
}

#[derive(Serialize)]
struct AuthorizationBoundaryReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    behavior: BoundaryBehaviorEvidence,
    limitations: [&'static str; 4],
}

struct LaneKey {
    signing: SigningKey,
    jwks_json: String,
}

/// Runs the boundary conformance and writes one evidence artifact.
pub fn run(service_binary: &Path, report_path: &Path) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for authorization boundary evidence")?;
    let lane = LaneKey::generate();
    let directory = env::temp_dir().join(format!("axiusflow-authz-lane-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    let jwks_path = directory.join("lane_jwks.json");
    let policy_path = directory.join("lane_policy_v1.json");
    fs::write(&jwks_path, &lane.jwks_json)?;
    fs::write(&policy_path, policy_document(1, true))?;

    let port = 19_000 + (std::process::id() % 1_000) as u16;
    let address = format!("127.0.0.1:{port}");
    let mut service = spawn_service(service_binary, &address, &jwks_path, &policy_path)?;
    let result = exercise(&lane, &address, &policy_path);
    let _ = service.kill();
    let output = service
        .wait_with_output()
        .map_err(|error| format!("service wait failed: {error}"))?;
    if result.is_err() {
        eprintln!(
            "authorization service stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let _ = fs::remove_dir_all(&directory);
    result?;

    let report = AuthorizationBoundaryReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        behavior: BoundaryBehaviorEvidence {
            startup: "passed",
            token_verification: "passed",
            allow_decision: "passed",
            deny_decision: "passed",
            expired_token_rejected: "passed",
            foreign_key_rejected: "passed",
            monotonic_policy_swap: "passed",
            revocation: "passed",
            malformed_rejected: "passed",
            oversized_rejected: "passed",
            tls: "not_exercised",
            sessions: "not_exercised",
            grpc_transport: "not_exercised",
        },
        limitations: [
            "plaintext_loopback",
            "no_session_issuance",
            "no_grpc_transport",
            "no_workload_identity",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "authorization_boundary=passed startup=true allow=true deny=true expired_rejected=true foreign_key_rejected=true revocation=true monotonic=true tls=not_exercised report={}",
        report_path.display()
    );
    Ok(())
}

fn exercise(lane: &LaneKey, address: &str, policy_path: &Path) -> Result<(), Box<dyn Error>> {
    wait_for_health(address)?;

    let valid_token = lane.mint_token("user-1", 3_600, None);
    let (status, body) = post_json(
        address,
        "/authorize",
        &json!({
            "token": valid_token,
            "resource_id": "bars:axf",
            "action": "stream",
            "correlation_id": "lane-allow-1"
        }),
    )?;
    require(
        status == 200 && body.contains("\"allowed\""),
        "granted stream was not allowed",
    )?;

    let (status, body) = post_json(
        address,
        "/authorize",
        &json!({
            "token": valid_token,
            "resource_id": "bars:unknown",
            "action": "stream",
            "correlation_id": "lane-deny-1"
        }),
    )?;
    require(
        status == 200 && body.contains("\"denied\""),
        "unknown resource was not denied",
    )?;

    let expired = lane.mint_token("user-1", 0, Some(-7_200));
    let (status, _) = post_json(
        address,
        "/authorize",
        &json!({
            "token": expired,
            "resource_id": "bars:axf",
            "action": "stream",
            "correlation_id": "lane-expired-1"
        }),
    )?;
    require(status == 401, "expired token was not rejected")?;

    let foreign = LaneKey::generate().mint_token("user-1", 3_600, None);
    let (status, _) = post_json(
        address,
        "/authorize",
        &json!({
            "token": foreign,
            "resource_id": "bars:axf",
            "action": "stream",
            "correlation_id": "lane-foreign-1"
        }),
    )?;
    require(status == 401, "foreign-key token was not rejected")?;

    let (status, _) = post_json(
        address,
        "/admin/policy",
        &json!({"version": 1, "grants": []}),
    )?;
    require(
        status == 409,
        "a non-increasing policy version was accepted",
    )?;

    fs::write(policy_path, policy_document(2, false))?;
    let (status, _) = post_json(
        address,
        "/admin/policy",
        &json!({"version": 2, "grants": []}),
    )?;
    require(status == 200, "monotonic policy swap failed")?;

    let (status, body) = post_json(
        address,
        "/authorize",
        &json!({
            "token": valid_token,
            "resource_id": "bars:axf",
            "action": "stream",
            "correlation_id": "lane-revoked-1"
        }),
    )?;
    require(
        status == 200 && body.contains("\"denied\""),
        "revoked grant was still allowed",
    )?;

    let (status, _) = post_raw(address, "/authorize", b"not json")?;
    require(status == 400, "malformed body was not rejected")?;
    let oversized = vec![b'x'; 8_192];
    let (status, _) = post_raw(address, "/authorize", &oversized)?;
    require(status == 413, "oversized body was not rejected")?;
    Ok(())
}

fn spawn_service(
    binary: &Path,
    address: &str,
    jwks_path: &Path,
    policy_path: &Path,
) -> Result<Child, Box<dyn Error>> {
    let child = Command::new(binary)
        .arg("--listen")
        .arg(address)
        .arg("--jwks")
        .arg(jwks_path)
        .arg("--policy")
        .arg(policy_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    Ok(child)
}

fn wait_for_health(address: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok((200, _)) = get(address, "/health") {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("authorization service did not become healthy".into())
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
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("{method} {path} connect failed: {error}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("{method} {path} head write failed: {error}"))?;
    stream
        .write_all(body)
        .map_err(|error| format!("{method} {path} body write failed: {error}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("{method} {path} shutdown failed: {error}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| format!("{method} {path} read failed: {error}"))?;
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

fn policy_document(version: u64, with_grant: bool) -> String {
    let grants = if with_grant {
        json!([{"grant_id": "g1", "principal_id": "user-1", "resource_id": "bars:axf", "action": "stream"}])
    } else {
        json!([])
    };
    json!({"version": version, "grants": grants}).to_string()
}

impl LaneKey {
    fn generate() -> Self {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).expect("operating-system entropy is available");
        let signing = SigningKey::from_bytes(&secret);
        let public = signing.verifying_key();
        let jwks_json = json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "alg": "EdDSA",
                "use": "sig",
                "kid": KEY_ID,
                "x": URL_SAFE_NO_PAD.encode(public.as_bytes()),
            }]
        })
        .to_string();
        Self { signing, jwks_json }
    }

    fn mint_token(
        &self,
        subject: &str,
        lifetime_seconds: i64,
        expired_offset: Option<i64>,
    ) -> String {
        let now = unix_seconds_now();
        let expiry = now + expired_offset.unwrap_or(lifetime_seconds);
        let header = URL_SAFE_NO_PAD
            .encode(json!({"alg": "EdDSA", "typ": "JWT", "kid": KEY_ID}).to_string());
        let claims = URL_SAFE_NO_PAD.encode(
            json!({
                "iss": "axiusflow",
                "aud": "authorization_service",
                "sub": subject,
                "exp": expiry,
                "purpose": "service_access",
                "session_id": "lane-session-1",
                "device_id": "lane-device-1"
            })
            .to_string(),
        );
        let input = format!("{header}.{claims}");
        let signature = self.signing.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }
}

fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().cast_signed())
        .unwrap_or_default()
}
