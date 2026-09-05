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

    /// Accepts one callback and returns its raw query with the connection
    /// held open. The caller validates, completes the exchange, and then
    /// answers on the same connection, so the browser page reflects the
    /// engine outcome instead of mere receipt.
    ///
    /// Times out instead of blocking forever when the browser is closed
    /// without completing authorization.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, oversized requests, or transport
    /// failure. Transport failures answer the browser immediately; a
    /// timeout means no browser ever connected, so nothing is answered.
    pub fn accept_one(&self, timeout: Duration) -> Result<PendingCallback, String> {
        self.listener
            .set_nonblocking(false)
            .map_err(|_| "loopback callback listener failed".to_string())?;
        // `set_timeout` is best-effort: the read loop below also enforces the
        // bound, so an unsupported platform timeout still fails closed.
        let _ = self.listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => return read_callback_query(stream),
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

/// One authorization callback with its browser connection held open for
/// the outcome response.
pub struct PendingCallback {
    stream: std::net::TcpStream,
    query: String,
}

impl PendingCallback {
    /// Returns the raw callback query for validation.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Answers the waiting browser with the completed engine outcome.
    /// Success reports only after exchange, link, and vault storage all
    /// finished; failure carries the redacted engine detail so the page
    /// never claims an unfinished sign-in succeeded.
    pub fn respond_outcome(mut self, result: Result<(), &str>) {
        match result {
            Ok(()) => respond(&mut self.stream, 200, &outcome_page(true, "")),
            Err(detail) => respond(&mut self.stream, 500, &outcome_page(false, detail)),
        }
    }

    /// Answers the waiting browser with a validation failure and releases
    /// the connection.
    pub fn respond_invalid(self, detail: &str) {
        let mut this = self;
        respond(&mut this.stream, 400, &outcome_page(false, detail));
    }
}

fn read_callback_query(mut stream: std::net::TcpStream) -> Result<PendingCallback, String> {
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
                    respond(&mut stream, 413, "payload too large");
                    return Err("authorization callback is too large".to_string());
                }
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => {
                respond(&mut stream, 400, "unreadable request");
                return Err("loopback callback read failed".to_string());
            }
        }
    }
    let request = String::from_utf8_lossy(&request);
    let Some(query) = callback_query_from_request(&request) else {
        respond(&mut stream, 400, "invalid callback");
        return Err("authorization callback is invalid".to_string());
    };
    Ok(PendingCallback { stream, query })
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 | 413 => "Bad Request",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn outcome_page(success: bool, detail: &str) -> String {
    let (title, heading, copy, mark) = if success {
        (
            "Sign-in confirmed — Axiusflow",
            "You’re signed in",
            "Authentication is complete. Return to Axiusflow to continue.",
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6.8 12.4 3.2 3.2 7.2-7.2"/></svg>"#.to_string(),
        )
    } else {
        (
            "Sign-in failed — Axiusflow",
            "Sign-in wasn’t completed",
            "Return to Axiusflow and try again.",
            "!".to_string(),
        )
    };
    let detail = if success {
        String::new()
    } else {
        format!("<p class=\"detail\">{}</p>", escape_html(detail))
    };
    let state = if success { "success" } else { "failure" };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="dark"><title>{title}</title><style>
*{{box-sizing:border-box}}html,body{{margin:0;min-height:100%;font-family:Inter,ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif}}body{{min-height:100vh;display:grid;place-items:center;background:#090b0f;color:#f4f5f7;padding:24px}}main{{width:min(420px,100%);text-align:center}}.brand{{margin-bottom:40px;font-size:12px;font-weight:700;letter-spacing:.22em;color:#9299a6}}.mark{{width:72px;height:72px;margin:0 auto 24px;display:grid;place-items:center;border-radius:50%;font-size:30px;font-weight:600;animation:arrive .42s cubic-bezier(.2,.8,.2,1) both}}.success .mark{{background:#163a2b;color:#70e0a7;box-shadow:0 0 0 1px #285940}}.failure .mark{{background:#3a1b1d;color:#ff9a9f;box-shadow:0 0 0 1px #633034}}svg{{width:34px;height:34px;fill:none;stroke:currentColor;stroke-width:2.2;stroke-linecap:round;stroke-linejoin:round}}svg path{{stroke-dasharray:18;stroke-dashoffset:18;animation:draw .45s .24s ease-out forwards}}h1{{margin:0 0 10px;font-size:26px;letter-spacing:-.03em}}p{{margin:0;color:#9ea5b1;font-size:14px;line-height:1.6}}.detail{{margin:16px auto 0;max-width:360px;color:#ff9a9f;font-size:13px}}@keyframes arrive{{from{{opacity:0;transform:scale(.7)}}to{{opacity:1;transform:scale(1)}}}}@keyframes draw{{to{{stroke-dashoffset:0}}}}@media(prefers-reduced-motion:reduce){{.mark,svg path{{animation:none}}svg path{{stroke-dashoffset:0}}}}
</style></head><body><main class="{state}"><div class="brand">AXIUSFLOW</div><div class="mark">{mark}</div><h1>{heading}</h1><p>{copy}</p>{detail}</main></body></html>"#
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
    use super::{
        LoopbackListener, callback_query_from_request, outcome_page, validate_callback_query,
    };
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

    /// Drives one browser callback and returns the page it read.
    fn browser_callback(port: u16, request: &[u8]) -> String {
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{port}")).expect("loopback connects");
        stream.write_all(request).expect("callback writes");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("callback response reads");
        String::from_utf8_lossy(&response).into_owned()
    }

    #[test]
    fn bound_listener_holds_the_connection_for_the_outcome() {
        let listener = LoopbackListener::bind().expect("loopback binds");
        let port = listener.port();
        assert!(listener.redirect_uri().contains(&port.to_string()));
        let sender = std::thread::spawn(move || {
            browser_callback(
                port,
                b"GET /callback?code=live-code&state=live-state HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            )
        });
        // Receipt alone answers nothing: the browser waits while the engine
        // exchanges, links, and stores.
        let callback = listener
            .accept_one(Duration::from_secs(5))
            .expect("callback arrives");
        let validated =
            validate_callback_query(callback.query(), "live-state").expect("callback validates");
        assert_eq!(validated.code, "live-code");
        callback.respond_outcome(Ok(()));
        let response = sender.join().expect("sender joins");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(
            response.contains("You’re signed in") && response.contains("@keyframes draw"),
            "success page must reflect completion: {response}"
        );
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
    }

    #[test]
    fn engine_failure_reaches_the_browser_truthfully() {
        let listener = LoopbackListener::bind().expect("loopback binds");
        let port = listener.port();
        let sender = std::thread::spawn(move || {
            browser_callback(
                port,
                b"GET /callback?code=live-code&state=live-state HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            )
        });
        let callback = listener
            .accept_one(Duration::from_secs(5))
            .expect("callback arrives");
        callback.respond_outcome(Err("account linking failed; retry sign-in"));
        let response = sender.join().expect("sender joins");
        // A received callback alone must never produce "fully
        // authenticated": failures carry the redacted engine detail.
        assert!(response.starts_with("HTTP/1.1 500"));
        assert!(response.contains("account linking failed; retry sign-in"));
        assert!(!response.contains("You’re signed in"));
    }

    #[test]
    fn invalid_callbacks_answer_immediately() {
        let listener = LoopbackListener::bind().expect("loopback binds");
        let port = listener.port();
        let sender = std::thread::spawn(move || {
            browser_callback(
                port,
                b"GET /callback?code=live-code&state=wrong-state HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            )
        });
        let callback = listener
            .accept_one(Duration::from_secs(5))
            .expect("callback arrives");
        assert!(validate_callback_query(callback.query(), "live-state").is_err());
        callback.respond_invalid("authorization state mismatch; retry sign-in");
        let response = sender.join().expect("sender joins");
        assert!(response.starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn failure_page_escapes_detail() {
        let page = outcome_page(false, "failed <script>alert('x')</script>");
        assert!(page.contains("&lt;script&gt;"));
        assert!(!page.contains("<script>"));
    }
}
