//! Bounded typed HTTP boundary for the authentication service.

use crate::credentials::{CredentialStore, maximum_token_lifetime_seconds};
use crate::issuance::IssuanceKey;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_REQUEST_BYTES: usize = 8_192;
const MAX_HEADER_LINES: usize = 32;
const MAX_BODY_BYTES: usize = 4_096;
const CONNECTION_DEADLINE: Duration = Duration::from_secs(5);
const ISSUER: &str = "axiusflow";
const AUDIENCE: &str = "authorization_service";

#[derive(Deserialize)]
struct SignupRequest {
    principal_id: String,
    password: String,
}

#[derive(Deserialize)]
struct SignInRequest {
    principal_id: String,
    password: String,
    device_id: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    token: String,
    expires_at_unix_seconds: u64,
    session_id: String,
}

/// The running authentication boundary.
pub struct AuthBoundary {
    key: IssuanceKey,
    client: Mutex<postgres::Client>,
}

impl AuthBoundary {
    /// Loads the signing key, connects, and applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns an error for key, connection, or migration failures.
    pub fn bootstrap(signing_key_path: &str, database_url: &str) -> Result<Self, String> {
        let key = IssuanceKey::load(signing_key_path, "auth-service-1", 1)?;
        let mut client = postgres::Client::connect(database_url, postgres::NoTls)
            .map_err(|error| format!("database connection failed: {error}"))?;
        axiusflow_persistence::migrate(&mut client, unix_nanos_now())
            .map_err(|error| format!("migration failed: {error}"))?;
        Ok(Self {
            key,
            client: Mutex::new(client),
        })
    }

    /// The signing key publication revision.
    #[must_use]
    pub const fn key_revision(&self) -> u64 {
        self.key.revision()
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
            drain_body(&mut reader, content_length, deadline);
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
        match (parts[0], parts[1]) {
            ("GET", "/health") => {
                let body = format!(
                    "{{\"key_revision\": {}, \"migrated\": true}}",
                    self.key_revision()
                );
                respond_json(&mut stream, 200, &body)
            }
            ("GET", "/jwks") => {
                let jwks = self.key.jwks_json();
                respond_json(&mut stream, 200, &jwks)
            }
            ("POST", "/signup") => self.handle_signup(&mut stream, &body),
            ("POST", "/sign_in") => self.handle_sign_in(&mut stream, &body),
            _ => respond(&mut stream, 404, "unknown route"),
        }
    }

    fn handle_signup(&self, stream: &mut TcpStream, body: &[u8]) -> Result<(), String> {
        let Ok(request) = serde_json::from_slice::<SignupRequest>(body) else {
            return respond(stream, 400, "malformed signup request");
        };
        let mut client = self
            .client
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        match CredentialStore::new(&mut client)
            .create_credential(&request.principal_id, &request.password)
        {
            Ok(()) => respond_json(stream, 201, "{\"created\": true}"),
            Err(_) => respond(stream, 400, "credential rejected"),
        }
    }

    fn handle_sign_in(&self, stream: &mut TcpStream, body: &[u8]) -> Result<(), String> {
        let Ok(request) = serde_json::from_slice::<SignInRequest>(body) else {
            return respond(stream, 400, "malformed sign-in request");
        };
        let mut client = self
            .client
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let opened = CredentialStore::new(&mut client)
            .open_session(&request.principal_id, &request.password)?;
        let Some((session, _secret)) = opened else {
            return respond(stream, 401, "invalid credentials");
        };
        drop(client);
        let expires_at = unix_seconds_now() + maximum_token_lifetime_seconds();
        let token = self.key.issue_token(
            &session.principal_id,
            ISSUER,
            AUDIENCE,
            expires_at,
            Some(&session.session_id),
            request.device_id.as_deref(),
        )?;
        let body = serde_json::to_string(&TokenResponse {
            token,
            expires_at_unix_seconds: expires_at,
            session_id: session.session_id,
        })
        .map_err(|error| error.to_string())?;
        respond_json(stream, 200, &body)
    }
}

fn drain_body(reader: &mut BufReader<TcpStream>, content_length: usize, deadline: Instant) {
    let mut remaining = content_length.min(MAX_BODY_BYTES * 4);
    let mut scratch = [0_u8; 4_096];
    while remaining > 0 && Instant::now() < deadline {
        let step = remaining.min(scratch.len());
        if reader.read_exact(&mut scratch[..step]).is_err() {
            break;
        }
        remaining -= step;
    }
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) -> Result<(), String> {
    let body = format!("{{\"error\": \"{message}\"}}");
    respond_json(stream, status, &body)
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), String> {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
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

fn unix_seconds_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn unix_nanos_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
