//! Bounded typed HTTP boundary for authorization decisions.

use crate::policy_source;
use axiusflow_authorization::{
    AuthorizationEvaluator, AuthorizationOutcome, AuthorizationRequest, PrincipalId, ResourceId,
};
use axiusflow_security::{
    Ed25519JwtVerifier, JwtVerificationRequest, JwtVerifier, SystemVerificationClock, TokenPurpose,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const MAX_REQUEST_BYTES: usize = 8_192;
const MAX_HEADER_LINES: usize = 32;
const MAX_BODY_BYTES: usize = 4_096;
const CONNECTION_DEADLINE: Duration = Duration::from_secs(2);
const EXPECTED_ISSUER: &str = "axiusflow";
const EXPECTED_AUDIENCE: &str = "authorization_service";

#[derive(Deserialize)]
struct AuthorizeRequest {
    token: String,
    resource_id: String,
    action: String,
    correlation_id: String,
}

#[derive(Serialize)]
struct AuthorizeResponse {
    outcome: String,
    policy_version: Option<u64>,
    grant_id: Option<String>,
    reason: String,
}

#[derive(Serialize)]
struct HealthResponse {
    policy_loaded: bool,
    policy_version: Option<u64>,
    key_revision: u64,
}

/// The running authorization boundary with a replaceable policy snapshot.
pub struct AuthorizationBoundary {
    verifier: Ed25519JwtVerifier<SystemVerificationClock>,
    evaluator: RwLock<AuthorizationEvaluator>,
}

impl AuthorizationBoundary {
    /// Builds the boundary from a verifier and the initial policy snapshot.
    #[must_use]
    pub fn new(
        verifier: Ed25519JwtVerifier<SystemVerificationClock>,
        policy: axiusflow_authorization::AuthorizationPolicySnapshot,
    ) -> Self {
        Self {
            verifier,
            evaluator: RwLock::new(AuthorizationEvaluator::with_snapshot(policy)),
        }
    }

    /// The installed policy version.
    #[must_use]
    pub fn policy_version(&self) -> u64 {
        self.evaluator.read().map_or(0, |evaluator| {
            evaluator
                .policy_version()
                .map_or(0, axiusflow_authorization::PolicyVersion::get)
        })
    }

    /// The JWKS revision in use.
    #[must_use]
    pub const fn key_revision(&self) -> u64 {
        self.verifier.key_set_revision()
    }

    /// Serves the bounded listener until shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener cannot bind or accept.
    pub fn serve(self, listen: &str) -> Result<(), String> {
        let listener = TcpListener::bind(listen).map_err(|error| error.to_string())?;
        let shared = Arc::new(self);
        for connection in listener.incoming() {
            let connection = connection.map_err(|error| error.to_string())?;
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                if let Err(error) = shared.handle_connection(connection) {
                    eprintln!("connection handler failed: {error}");
                }
            });
        }
        Ok(())
    }

    fn handle_connection(&self, mut stream: TcpStream) -> Result<(), String> {
        stream
            .set_read_timeout(Some(CONNECTION_DEADLINE))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(CONNECTION_DEADLINE))
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + CONNECTION_DEADLINE;
        let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .map_err(|error| error.to_string())?;
        if request_line.len() > MAX_REQUEST_BYTES {
            return respond(&mut stream, 413, "request line too large");
        }
        let mut content_length = 0_usize;
        let mut header_lines = 0_usize;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            if line == "\r\n" || line.is_empty() {
                break;
            }
            header_lines += 1;
            if header_lines > MAX_HEADER_LINES {
                return respond(&mut stream, 413, "too many headers");
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid content length".to_string())?;
            }
        }
        if Instant::now() >= deadline {
            return respond(&mut stream, 408, "deadline exceeded");
        }
        if content_length > MAX_BODY_BYTES {
            // Drain the in-flight body before answering; closing with unread data
            // would reset the client and discard the 413.
            let mut remaining = content_length.min(MAX_BODY_BYTES * 4);
            let mut scratch = [0_u8; 4_096];
            while remaining > 0 && Instant::now() < deadline {
                let step = remaining.min(scratch.len());
                if reader.read_exact(&mut scratch[..step]).is_err() {
                    break;
                }
                remaining -= step;
            }
            return respond(&mut stream, 413, "body too large");
        }
        let mut body = vec![0_u8; content_length];
        reader
            .read_exact(&mut body)
            .map_err(|error| error.to_string())?;

        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() != 3 {
            return respond(&mut stream, 400, "malformed request line");
        }
        let (method, path) = (parts[0], parts[1]);
        match (method, path) {
            ("GET", "/health") => self.handle_health(&mut stream),
            ("POST", "/authorize") => self.handle_authorize(&mut stream, &body),
            ("POST", "/admin/policy") => self.handle_policy_swap(&mut stream, &body),
            _ => respond(&mut stream, 404, "unknown route"),
        }
    }

    fn handle_health(&self, stream: &mut TcpStream) -> Result<(), String> {
        let version = self.evaluator.read().ok().and_then(|evaluator| {
            evaluator
                .policy_version()
                .map(axiusflow_authorization::PolicyVersion::get)
        });
        let body = serde_json::to_string(&HealthResponse {
            policy_loaded: version.is_some(),
            policy_version: version,
            key_revision: self.key_revision(),
        })
        .map_err(|error| error.to_string())?;
        respond_json(stream, 200, &body)
    }

    fn handle_authorize(&self, stream: &mut TcpStream, body: &[u8]) -> Result<(), String> {
        let request: AuthorizeRequest = match serde_json::from_slice(body) {
            Ok(request) => request,
            Err(_) => return respond(stream, 400, "malformed authorization request"),
        };
        let identity = self.verifier.verify(JwtVerificationRequest {
            encoded_token: &request.token,
            expected_issuer: EXPECTED_ISSUER,
            expected_audience: EXPECTED_AUDIENCE,
            purpose: TokenPurpose::ServiceAccess,
        });
        let Ok(identity) = identity else {
            return respond(stream, 401, "token verification failed");
        };
        let Ok(principal) = PrincipalId::try_new(identity.subject_id) else {
            return respond(stream, 401, "token subject is not a valid principal");
        };
        let Ok(resource) = ResourceId::try_new(request.resource_id) else {
            return respond(stream, 400, "invalid resource id");
        };
        let action = match request.action.as_str() {
            "read" => axiusflow_authorization::AuthorizationAction::Read,
            "stream" => axiusflow_authorization::AuthorizationAction::Stream,
            "trade" => axiusflow_authorization::AuthorizationAction::Trade,
            "administer" => axiusflow_authorization::AuthorizationAction::Administer,
            _ => return respond(stream, 400, "unknown action"),
        };
        let Ok(request) =
            AuthorizationRequest::try_new(principal, resource, action, request.correlation_id)
        else {
            return respond(stream, 400, "invalid correlation id");
        };
        let decision = self
            .evaluator
            .read()
            .map_err(|_| "evaluator lock poisoned".to_string())?
            .evaluate(&request);
        let (outcome, grant_id) = match decision.outcome() {
            AuthorizationOutcome::Allowed => ("allowed", None),
            AuthorizationOutcome::AdditionalAssuranceRequired => {
                ("additional_assurance_required", None)
            }
            AuthorizationOutcome::Denied => ("denied", None),
        };
        let body = serde_json::to_string(&AuthorizeResponse {
            outcome: outcome.to_string(),
            policy_version: decision
                .policy_version()
                .map(axiusflow_authorization::PolicyVersion::get),
            grant_id,
            reason: format!("{:?}", decision.reason()),
        })
        .map_err(|error| error.to_string())?;
        respond_json(stream, 200, &body)
    }

    fn handle_policy_swap(&self, stream: &mut TcpStream, body: &[u8]) -> Result<(), String> {
        let Ok(content) = std::str::from_utf8(body) else {
            return respond(stream, 400, "policy document is not UTF-8");
        };
        let Ok(snapshot) = policy_source::parse_policy(content) else {
            return respond(stream, 400, "invalid policy document");
        };
        let mut evaluator = self
            .evaluator
            .write()
            .map_err(|_| "evaluator lock poisoned".to_string())?;
        let current = evaluator
            .policy_version()
            .map(axiusflow_authorization::PolicyVersion::get);
        if current.is_some_and(|current| snapshot.version().get() <= current) {
            return respond(stream, 409, "policy version must increase monotonically");
        }
        let installed = snapshot.version().get();
        *evaluator = AuthorizationEvaluator::with_snapshot(snapshot);
        let body = format!("{{\"policy_version\": {installed}}}");
        respond_json(stream, 200, &body)
    }
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) -> Result<(), String> {
    let body = format!("{{\"error\": \"{message}\"}}");
    respond_json(stream, status, &body)
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), String> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| error.to_string())
}
