//! Bounded native PKCE loopback redirect listener.
//!
//! The listener binds an ephemeral `127.0.0.1` port, accepts exactly one authorization
//! redirect, and returns the authorization code only when the echoed `state` matches the
//! generated secret. Every limit is explicit: request bytes, header count, and accept
//! deadline are bounded so a hostile or stalled browser cannot exhaust the client.

use crate::pkce::PkceSecret;
use core::fmt;
use std::error::Error;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

/// Largest request line accepted from the browser redirect.
const MAXIMUM_REQUEST_LINE_BYTES: usize = 8_192;
/// Largest number of header lines drained before the request is rejected.
const MAXIMUM_HEADER_LINES: usize = 64;
/// Largest single header line accepted from the browser redirect.
const MAXIMUM_HEADER_LINE_BYTES: usize = 8_192;
/// Interval between non-blocking accept attempts while awaiting the redirect.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Largest sanitized authorization-server error code retained for reporting.
const MAXIMUM_SERVER_ERROR_BYTES: usize = 64;

/// Reason a loopback authorization redirect did not yield a usable code.
#[derive(Debug)]
pub enum LoopbackCallbackError {
    Bind(std::io::Error),
    Accept(std::io::Error),
    Read(std::io::Error),
    Write(std::io::Error),
    DeadlineExceeded,
    RequestLineTooLong,
    HeaderTooLong,
    TooManyHeaders,
    MalformedRequestLine,
    UnsupportedMethod,
    RedirectPathMismatch,
    MissingAuthorizationCode,
    MissingState,
    StateMismatch,
    AuthorizationServerError(String),
}

impl fmt::Display for LoopbackCallbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(formatter, "loopback redirect bind failed: {error}"),
            Self::Accept(error) => write!(formatter, "loopback redirect accept failed: {error}"),
            Self::Read(error) => write!(formatter, "loopback redirect read failed: {error}"),
            Self::Write(error) => write!(formatter, "loopback redirect write failed: {error}"),
            Self::DeadlineExceeded => {
                formatter.write_str("loopback redirect deadline elapsed before a request arrived")
            }
            Self::RequestLineTooLong => {
                formatter.write_str("redirect request line exceeded its bound")
            }
            Self::HeaderTooLong => formatter.write_str("redirect header line exceeded its bound"),
            Self::TooManyHeaders => {
                formatter.write_str("redirect sent more headers than permitted")
            }
            Self::MalformedRequestLine => {
                formatter.write_str("redirect request line was malformed")
            }
            Self::UnsupportedMethod => formatter.write_str("redirect used a method other than GET"),
            Self::RedirectPathMismatch => {
                formatter.write_str("redirect targeted a path this listener does not own")
            }
            Self::MissingAuthorizationCode => {
                formatter.write_str("redirect omitted the authorization code")
            }
            Self::MissingState => formatter.write_str("redirect omitted the CSRF state"),
            Self::StateMismatch => {
                formatter.write_str("redirect state did not match the generated PKCE state")
            }
            Self::AuthorizationServerError(error) => {
                write!(formatter, "authorization server reported: {error}")
            }
        }
    }
}

impl LoopbackCallbackError {
    /// Returns whether this failure came from a caller that proved knowledge of `state`.
    ///
    /// Only such a failure may abort the pending sign-in. Everything else is a stray or
    /// hostile request that must not deny the flow.
    const fn proves_state_knowledge(&self) -> bool {
        matches!(
            self,
            Self::AuthorizationServerError(_) | Self::MissingAuthorizationCode
        )
    }
}

impl Error for LoopbackCallbackError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Bind(error) | Self::Accept(error) | Self::Read(error) | Self::Write(error) => {
                Some(error)
            }
            _ => None,
        }
    }
}

/// Authorization code captured from a verified loopback redirect.
///
/// `Debug` redacts the code so it cannot reach logs before the token exchange.
#[derive(Clone)]
pub struct AuthorizationCode(String);

impl AuthorizationCode {
    /// Returns the single-use authorization code.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthorizationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AuthorizationCode")
            .field(&"<redacted>")
            .finish()
    }
}

/// A bound loopback listener awaiting exactly one authorization redirect.
#[derive(Debug)]
pub struct LoopbackRedirectListener {
    listener: TcpListener,
    redirect_path: String,
}

impl LoopbackRedirectListener {
    /// Binds an ephemeral loopback port that will serve `redirect_path` once.
    ///
    /// # Errors
    ///
    /// Returns [`LoopbackCallbackError::Bind`] when the loopback port cannot be bound.
    pub fn bind(redirect_path: &str) -> Result<Self, LoopbackCallbackError> {
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        let listener = TcpListener::bind(address).map_err(LoopbackCallbackError::Bind)?;
        Ok(Self {
            listener,
            redirect_path: redirect_path.to_string(),
        })
    }

    /// Returns the bound loopback address to register as the redirect URI.
    ///
    /// # Errors
    ///
    /// Returns [`LoopbackCallbackError::Bind`] when the bound address cannot be queried.
    pub fn local_addr(&self) -> Result<SocketAddr, LoopbackCallbackError> {
        self.listener
            .local_addr()
            .map_err(LoopbackCallbackError::Bind)
    }

    /// Returns the exact redirect URI the authorization request must declare.
    ///
    /// # Errors
    ///
    /// Returns [`LoopbackCallbackError::Bind`] when the bound address cannot be queried.
    pub fn redirect_uri(&self) -> Result<String, LoopbackCallbackError> {
        let address = self.local_addr()?;
        Ok(format!(
            "http://127.0.0.1:{}{}",
            address.port(),
            self.redirect_path
        ))
    }

    /// Serves redirects until one proves knowledge of `state`, or the deadline elapses.
    ///
    /// The listener is consumed so a single secret can never be reused across redirects.
    /// A request that fails `state` verification, targets another path, uses another method,
    /// or exceeds a bound is answered with `400` and does **not** abort the pending sign-in,
    /// so a stray local connection or a hostile page cannot deny the flow. Only a
    /// `state`-verified response resolves the wait.
    ///
    /// # Errors
    ///
    /// Returns [`LoopbackCallbackError::DeadlineExceeded`] when no `state`-verified redirect
    /// arrives in time, [`LoopbackCallbackError::AuthorizationServerError`] when the
    /// authorization server reported a failure for this exact `state`, or
    /// [`LoopbackCallbackError::MissingAuthorizationCode`] when a verified redirect carried
    /// no authorization code.
    pub fn accept_authorization_code(
        self,
        secret: &PkceSecret,
        deadline: Duration,
    ) -> Result<AuthorizationCode, LoopbackCallbackError> {
        let expiry = Instant::now()
            .checked_add(deadline)
            .ok_or(LoopbackCallbackError::DeadlineExceeded)?;
        self.listener
            .set_nonblocking(true)
            .map_err(LoopbackCallbackError::Accept)?;

        loop {
            let Some(remaining) = expiry
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
            else {
                return Err(LoopbackCallbackError::DeadlineExceeded);
            };
            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    match self.serve_redirect(&mut stream, secret, remaining) {
                        Ok(code) => return Ok(code),
                        Err(error) if error.proves_state_knowledge() => return Err(error),
                        Err(_) => {}
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL_INTERVAL.min(remaining));
                }
                Err(error) => return Err(LoopbackCallbackError::Accept(error)),
            }
        }
    }

    /// Serves one accepted connection and answers the browser exactly once.
    fn serve_redirect(
        &self,
        stream: &mut TcpStream,
        secret: &PkceSecret,
        remaining: Duration,
    ) -> Result<AuthorizationCode, LoopbackCallbackError> {
        stream
            .set_nonblocking(false)
            .map_err(LoopbackCallbackError::Read)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(LoopbackCallbackError::Read)?;
        stream
            .set_write_timeout(Some(remaining))
            .map_err(LoopbackCallbackError::Write)?;

        let outcome = self.read_authorization_code(stream, secret);
        let (status, body) = if outcome.is_ok() {
            (
                "200 OK",
                "Axiusflow sign-in complete. Return to the application.",
            )
        } else {
            (
                "400 Bad Request",
                "Axiusflow sign-in failed. Return to the application.",
            )
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .map_err(LoopbackCallbackError::Write)?;
        stream.flush().map_err(LoopbackCallbackError::Write)?;
        outcome
    }

    fn read_authorization_code(
        &self,
        stream: &mut TcpStream,
        secret: &PkceSecret,
    ) -> Result<AuthorizationCode, LoopbackCallbackError> {
        let mut reader = BufReader::new(Read::by_ref(stream));
        let mut request_line = String::new();
        read_bounded_line(
            &mut reader,
            &mut request_line,
            MAXIMUM_REQUEST_LINE_BYTES,
            LoopbackCallbackError::RequestLineTooLong,
        )?;
        drain_bounded_headers(&mut reader)?;

        let mut parts = request_line.split_whitespace();
        let method = parts
            .next()
            .ok_or(LoopbackCallbackError::MalformedRequestLine)?;
        let target = parts
            .next()
            .ok_or(LoopbackCallbackError::MalformedRequestLine)?;
        if method != "GET" {
            return Err(LoopbackCallbackError::UnsupportedMethod);
        }
        let (path, query) = target
            .split_once('?')
            .map_or((target, ""), |(path, query)| (path, query));
        if path != self.redirect_path {
            return Err(LoopbackCallbackError::RedirectPathMismatch);
        }

        let mut code = None;
        let mut state = None;
        let mut server_error = None;
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name {
                "code" => code = Some(percent_decode(value)),
                "state" => state = Some(percent_decode(value)),
                "error" => server_error = Some(percent_decode(value)),
                _ => {}
            }
        }

        let state = state.ok_or(LoopbackCallbackError::MissingState)?;
        if !constant_time_equals(state.as_bytes(), secret.state().as_bytes()) {
            return Err(LoopbackCallbackError::StateMismatch);
        }
        // RFC 6749 section 4.1.2.1 requires verifying `state` on error responses too, so an
        // unverified caller can neither abort the flow nor inject text into diagnostics.
        if let Some(error) = server_error {
            return Err(LoopbackCallbackError::AuthorizationServerError(
                sanitize_server_error(&error),
            ));
        }
        let code = code.ok_or(LoopbackCallbackError::MissingAuthorizationCode)?;
        if code.is_empty() {
            return Err(LoopbackCallbackError::MissingAuthorizationCode);
        }
        Ok(AuthorizationCode(code))
    }
}

fn read_bounded_line(
    reader: &mut BufReader<&mut TcpStream>,
    destination: &mut String,
    limit: usize,
    overflow: LoopbackCallbackError,
) -> Result<(), LoopbackCallbackError> {
    let mut bounded = reader.take(limit as u64 + 1);
    let mut raw = Vec::new();
    bounded
        .read_until(b'\n', &mut raw)
        .map_err(LoopbackCallbackError::Read)?;
    if raw.len() > limit {
        return Err(overflow);
    }
    destination.push_str(
        core::str::from_utf8(&raw)
            .map_err(|_| LoopbackCallbackError::MalformedRequestLine)?
            .trim_end_matches(['\r', '\n']),
    );
    Ok(())
}

fn drain_bounded_headers(
    reader: &mut BufReader<&mut TcpStream>,
) -> Result<(), LoopbackCallbackError> {
    for _ in 0..MAXIMUM_HEADER_LINES {
        let mut header = String::new();
        read_bounded_line(
            reader,
            &mut header,
            MAXIMUM_HEADER_LINE_BYTES,
            LoopbackCallbackError::HeaderTooLong,
        )?;
        if header.is_empty() {
            return Ok(());
        }
    }
    Err(LoopbackCallbackError::TooManyHeaders)
}

/// Decodes `application/x-www-form-urlencoded` query values.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = (bytes[index + 1] as char).to_digit(16);
                let low = (bytes[index + 2] as char).to_digit(16);
                if let (Some(high), Some(low)) = (high, low) {
                    decoded.push(u8::try_from(high * 16 + low).unwrap_or(b'?'));
                    index += 3;
                } else {
                    decoded.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Reduces an authorization-server `error` value to a bounded RFC 6749 error code.
///
/// RFC 6749 section 4.1.2.1 error codes are lowercase ASCII with underscores. Anything
/// else is dropped so a redirect cannot inject arbitrary text into diagnostics or logs.
fn sanitize_server_error(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(MAXIMUM_SERVER_ERROR_BYTES)
        .collect()
}

/// Compares two byte strings without leaking their contents through timing.
fn constant_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
