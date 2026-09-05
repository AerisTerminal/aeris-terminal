//! Ephemeral loopback callback listener for one native login transaction.
//!
//! The listener binds literal `127.0.0.1` on an OS-assigned port, accepts
//! exactly one authorization callback, validates it against the pending
//! transaction, and then closes. It never binds all interfaces, never serves
//! more than one transaction, and every read is bounded.

use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

/// Maximum HTTP callback request bytes read from the loopback socket.
pub const MAXIMUM_CALLBACK_BYTES: usize = 8192;
/// Maximum authorization code length accepted from the callback query.
pub const MAXIMUM_CODE_BYTES: usize = 2048;
/// Maximum OAuth state length accepted from the callback query.
pub const MAXIMUM_STATE_BYTES: usize = 256;

/// One bound loopback listener awaiting a single callback.
pub struct LoopbackListener {
    listener: TcpListener,
    port: u16,
}

impl LoopbackListener {
    /// Binds literal `127.0.0.1` on an OS-assigned port.
    ///
    /// # Errors
    ///
    /// Returns an error when the loopback bind fails (port collision or
    /// restricted loopback), with an actionable detail.
    pub fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "loopback callback port is unavailable; retry sign-in".to_string())?;
        let port = listener
            .local_addr()
            .map_err(|_| "loopback callback port is unavailable; retry sign-in".to_string())?
            .port();
        Ok(Self { listener, port })
    }

    /// Returns the OS-assigned loopback port for the redirect URI.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the redirect URI registered for this transaction.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Accepts one callback and returns its raw query string.
    ///
    /// Times out instead of blocking forever when the browser is closed
    /// without completing authorization.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, oversized requests, or transport failure.
    pub fn accept_one(&self, timeout: Duration) -> Result<String, String> {
        self.listener
            .set_nonblocking(false)
            .map_err(|_| "loopback callback listener failed".to_string())?;
        // `set_timeout` is best-effort: the read loop below also enforces the
        // bound, so an unsupported platform timeout still fails closed.
        let _ = self.listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => return read_callback_query(&mut stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(
                            "sign-in timed out waiting for the browser; retry sign-in".to_string()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => {
                    return Err("loopback callback connection failed; retry sign-in".to_string());
                }
            }
        }
    }
}

fn read_callback_query(stream: &mut std::net::TcpStream) -> Result<String, String> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "loopback callback read failed".to_string())?;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                request.extend_from_slice(&chunk[..read]);
                if request.len() > MAXIMUM_CALLBACK_BYTES {
                    respond(stream, 413, "payload too large");
                    return Err("authorization callback is too large".to_string());
                }
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => {
                respond(stream, 400, "unreadable request");
                return Err("loopback callback read failed".to_string());
            }
        }
    }
    let request = String::from_utf8_lossy(&request);
    let Some(query) = callback_query_from_request(&request) else {
        respond(stream, 400, "invalid callback");
        return Err("authorization callback is invalid".to_string());
    };
    // The callback only proves the browser returned: exchange, link, and
    // vault storage still follow. The desktop reports success only after
    // those complete, so this page must not claim the sign-in finished.
    respond(
        stream,
        200,
        "Authorization received. Return to Axiusflow to confirm sign-in.",
    );
    Ok(query)
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn callback_query_from_request(request: &str) -> Option<String> {
    let line = request.lines().next()?;
    let path = line.strip_prefix("GET ")?;
    let path = path.split_whitespace().next()?;
    let (_, query) = path.split_once('?')?;
    if path.starts_with("/callback?") && !query.is_empty() {
        Some(query.to_string())
    } else {
        None
    }
}

/// One validated authorization callback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedCallback {
    /// Authorization code for the token exchange.
    pub code: String,
    /// OAuth state echoed by the control plane.
    pub state: String,
}

/// Validates a raw callback query against the pending transaction state.
///
/// # Errors
///
/// Returns a redacted error when the query is malformed, the code is
/// missing, or the state mismatches (possible cross-request forgery).
pub fn validate_callback_query(
    query: &str,
    expected_state: &str,
) -> Result<ValidatedCallback, String> {
    if query.len() > MAXIMUM_CALLBACK_BYTES {
        return Err("authorization callback is too large".to_string());
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => {
                if code.is_some() {
                    return Err("authorization callback is invalid".to_string());
                }
                code = Some(value);
            }
            "state" => {
                if state.is_some() {
                    return Err("authorization callback is invalid".to_string());
                }
                state = Some(value);
            }
            "error" => {
                error = Some(value);
            }
            _ => {}
        }
    }
    if let Some(provider_error) = error {
        let _ = provider_error;
        return Err("sign-in was declined or cancelled in the browser".to_string());
    }
    let (Some(code), Some(state)) = (code, state) else {
        return Err("authorization callback is invalid".to_string());
    };
    if code.is_empty() || code.len() > MAXIMUM_CODE_BYTES {
        return Err("authorization callback is invalid".to_string());
    }
    if state.len() > MAXIMUM_STATE_BYTES || state != expected_state {
        return Err("authorization state mismatch; retry sign-in".to_string());
    }
    Ok(ValidatedCallback {
        code: code.to_string(),
        state: state.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{LoopbackListener, callback_query_from_request, validate_callback_query};
    use std::{
        io::{Read, Write},
        net::TcpStream,
        time::Duration,
    };

    #[test]
    fn callback_path_parses_its_query() {
        let request = "GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(
            callback_query_from_request(request),
            Some("code=abc&state=xyz".to_string())
        );
        assert_eq!(
            callback_query_from_request("GET /other?code=abc HTTP/1.1\r\n\r\n"),
            None
        );
    }

    #[test]
    fn mismatched_state_and_missing_code_fail_closed() {
        assert!(
            validate_callback_query("code=abc&state=wrong", "expected")
                .expect_err("mismatch fails")
                .contains("mismatch")
        );
        assert!(validate_callback_query("state=expected", "expected").is_err());
        assert!(validate_callback_query("code=&state=expected", "expected").is_err());
        assert!(validate_callback_query("code=a&code=b&state=expected", "expected").is_err());
        assert!(validate_callback_query("error=access_denied&state=expected", "expected").is_err());
    }

    #[test]
    fn valid_callback_passes_through() {
        let validated =
            validate_callback_query("code=abc123&state=s3", "s3").expect("valid callback passes");
        assert_eq!(validated.code, "abc123");
    }

    #[test]
    fn bound_listener_accepts_one_browser_callback_over_loopback() {
        let listener = LoopbackListener::bind().expect("loopback binds");
        let port = listener.port();
        assert!(listener.redirect_uri().contains(&port.to_string()));
        let sender = std::thread::spawn(move || {
            let mut stream =
                TcpStream::connect(format!("127.0.0.1:{port}")).expect("loopback connects");
            stream
                .write_all(
                    b"GET /callback?code=live-code&state=live-state HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .expect("callback writes");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("callback response reads");
            String::from_utf8_lossy(&response).into_owned()
        });
        let query = listener
            .accept_one(Duration::from_secs(5))
            .expect("callback arrives");
        let validated = validate_callback_query(&query, "live-state").expect("callback validates");
        assert_eq!(validated.code, "live-code");
        let response = sender.join().expect("sender joins");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        // The page reports receipt, never completion: exchange, link, and
        // vault storage still follow on the worker.
        assert!(
            response.contains("Authorization received. Return to Axiusflow to confirm sign-in."),
            "callback page must not claim success: {response}"
        );
        assert!(!response.to_lowercase().contains("complete"));
    }
}
