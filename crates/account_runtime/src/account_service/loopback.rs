//! Loopback callback listener for one native login transaction.
//!
//! The listener binds the registered fixed callback ports in order on both
//! loopback families (`127.0.0.1` and `[::1]`), so the `localhost` redirect
//! reaches it however the browser resolves that name. It accepts one
//! authorization callback, validates it against the pending transaction, and
//! closes with the transaction. It never binds all interfaces, ignores a
//! bounded number of stray connections, and every read is bounded.

use std::{
    io::{ErrorKind, Read, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};

/// Callback ports registered with the identity provider, tried in order.
pub const CALLBACK_PORTS: [u16; 3] = [47210, 47211, 47212];
/// Maximum HTTP callback request bytes read from the loopback socket.
pub const MAXIMUM_CALLBACK_BYTES: usize = 8192;
/// Maximum authorization code length accepted from the callback query.
pub const MAXIMUM_CODE_BYTES: usize = 2048;
/// Maximum OAuth state length accepted from the callback query.
pub const MAXIMUM_STATE_BYTES: usize = 256;
/// Connections that are not the callback (speculative preconnects, favicon
/// requests) answered before the transaction gives up on this listener.
const MAXIMUM_STRAY_CONNECTIONS: usize = 16;
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

const PLATFORM_STYLESHEET: &str = include_str!("../../../ui/design_system/platform.css");
/// DM Sans faces at the weights `platform.css` uses for normal, emphasis and strong text.
const PLATFORM_FONTS: [(&[u8], u16); 3] = [
    (
        include_bytes!("../../../ui/design_system/assets/fonts/dm_sans_regular.ttf"),
        400,
    ),
    (
        include_bytes!("../../../ui/design_system/assets/fonts/dm_sans_medium.ttf"),
        500,
    ),
    (
        include_bytes!("../../../ui/design_system/assets/fonts/dm_sans_bold.ttf"),
        700,
    ),
];
const BRAND_FONT: &[u8] = include_bytes!(
    "../../../ui/design_system/assets/fonts/faculty-glyphic/FacultyGlyphic-Regular.ttf"
);
const BRAND_MARK: &str = include_str!("../../../../apps/desktop/assets/aeris_assets/logo.svg");
const SYSTEM_THEME_BOOTSTRAP: &str = r"<script>(function(){var q=window.matchMedia('(prefers-color-scheme: dark)');function apply(){var d=q.matches;document.documentElement.classList.toggle('dark',d);document.documentElement.dataset.theme=d?'dark':'light';document.documentElement.style.colorScheme=d?'dark':'light'}apply();if(q.addEventListener){q.addEventListener('change',apply)}else if(q.addListener){q.addListener(apply)}})();</script>";

/// One bound loopback listener pair awaiting a single callback.
pub struct LoopbackListener {
    listeners: Vec<TcpListener>,
    port: u16,
}

impl LoopbackListener {
    /// Binds the first port in `ports` (normally [`CALLBACK_PORTS`]) that is
    /// free on IPv4 loopback and, when the host has IPv6 loopback, on `[::1]`
    /// as well. Port `0` asks the OS for any free port so parallel tests never
    /// contend for the registered ports.
    ///
    /// # Errors
    ///
    /// Returns an actionable error when every listed port is in use.
    pub fn bind(ports: &[u16]) -> Result<Self, String> {
        for &port in ports {
            let Ok(ipv4) = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))) else {
                continue;
            };
            let Ok(bound) = ipv4.local_addr().map(|address| address.port()) else {
                continue;
            };
            let mut listeners = vec![ipv4];
            match TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, bound))) {
                Ok(ipv6) => listeners.push(ipv6),
                // Another process owns the IPv6 side: a `localhost` redirect
                // could reach it instead of this transaction.
                Err(error) if error.kind() == ErrorKind::AddrInUse => continue,
                // Hosts without IPv6 loopback resolve `localhost` to IPv4 only.
                Err(_) => {}
            }
            if listeners
                .iter()
                .any(|listener| listener.set_nonblocking(true).is_err())
            {
                return Err("loopback callback listener failed; retry sign-in".to_string());
            }
            return Ok(Self {
                listeners,
                port: bound,
            });
        }
        Err(format!(
            "sign-in callback ports {} are in use; close the app using them and retry sign-in",
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Returns the bound loopback port.
    #[cfg(test)]
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the redirect URI registered for this transaction.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}/callback", self.port)
    }

    /// Accepts one callback and returns its raw query with the connection
    /// held open. The caller validates, completes the exchange, and then
    /// answers on the same connection, so the browser page reflects the
    /// runtime outcome instead of mere receipt.
    #[cfg(test)]
    pub fn accept_one(&self, timeout: Duration) -> Result<PendingCallback, String> {
        self.accept_one_while(timeout, || true)
    }

    /// Accepts one callback while the owning login transaction remains
    /// current. The predicate is checked between nonblocking accept attempts,
    /// so cancellation retires the listener promptly instead of retaining a
    /// socket and worker until the full login timeout. Times out instead of
    /// blocking forever when the browser is closed without completing
    /// authorization.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, cancellation, too many stray connections,
    /// or listener failure.
    pub fn accept_one_while(
        &self,
        timeout: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Result<PendingCallback, String> {
        let deadline = Instant::now() + timeout;
        let mut stray_connections = 0;
        loop {
            if !keep_waiting() {
                return Err("sign-in transaction is no longer active; retry sign-in".to_string());
            }
            let mut accepted = None;
            for listener in &self.listeners {
                match listener.accept() {
                    Ok((stream, _)) => {
                        accepted = Some(stream);
                        break;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => {
                        return Err(
                            "loopback callback connection failed; retry sign-in".to_string()
                        );
                    }
                }
            }
            if let Some(stream) = accepted {
                if let Some(callback) = read_callback_query(stream) {
                    return Ok(callback);
                }
                stray_connections += 1;
                if stray_connections >= MAXIMUM_STRAY_CONNECTIONS {
                    return Err("authorization callback is invalid; retry sign-in".to_string());
                }
                continue;
            }
            if Instant::now() >= deadline {
                return Err("sign-in timed out waiting for the browser; retry sign-in".to_string());
            }
            std::thread::sleep(ACCEPT_POLL_INTERVAL);
        }
    }
}

/// One authorization callback with its browser connection held open for
/// the outcome response.
pub struct PendingCallback {
    stream: TcpStream,
    query: String,
}

impl PendingCallback {
    /// Returns the raw callback query for validation.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Answers the waiting browser with the completed runtime outcome.
    /// Success reports only after exchange, verification, and vault storage
    /// all finished; failure carries the redacted runtime detail so the page
    /// never claims an unfinished sign-in succeeded.
    pub fn respond_outcome(mut self, result: Result<(), &str>) {
        match result {
            Ok(()) => respond(&mut self.stream, 200, &outcome_page(true, "")),
            Err(detail) => respond(&mut self.stream, 500, &outcome_page(false, detail)),
        }
    }

    /// Answers the waiting browser with a validation failure and releases
    /// the connection.
    pub fn respond_invalid(mut self, detail: &str) {
        respond(&mut self.stream, 400, &outcome_page(false, detail));
    }
}

/// Reads one request head. Returns `None` after answering any connection
/// that is not a bounded `GET /callback?…` request.
fn read_callback_query(mut stream: TcpStream) -> Option<PendingCallback> {
    // BSD-derived systems hand out accepted sockets that inherit the
    // listener's nonblocking mode; reads and the large outcome write must
    // block within their timeouts on every platform.
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(CALLBACK_READ_TIMEOUT))
            .is_err()
        || stream
            .set_write_timeout(Some(CALLBACK_READ_TIMEOUT))
            .is_err()
    {
        return None;
    }
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                request.extend_from_slice(&chunk[..read]);
                if request.len() > MAXIMUM_CALLBACK_BYTES {
                    respond(&mut stream, 413, "payload too large");
                    return None;
                }
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    let request = String::from_utf8_lossy(&request);
    let Some(query) = callback_query_from_request(&request) else {
        if !request.is_empty() {
            respond(&mut stream, 404, "not found");
        }
        return None;
    };
    Some(PendingCallback { stream, query })
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn browser_platform_styles() -> &'static str {
    PLATFORM_STYLESHEET
        .find(":root")
        .map_or(PLATFORM_STYLESHEET, |start| &PLATFORM_STYLESHEET[start..])
}

fn browser_font_faces() -> String {
    PLATFORM_FONTS
        .iter()
        .map(|&(face, weight)| ("DM Sans", face, weight))
        .chain([("Faculty Glyphic", BRAND_FONT, 400)])
        .map(|(family, face, weight)| {
            let face = STANDARD.encode(face);
            format!(
                r#"@font-face{{font-family:"{family}";src:url("data:font/ttf;base64,{face}") format("truetype");font-style:normal;font-weight:{weight};font-display:swap}}"#
            )
        })
        .collect::<Vec<_>>()
        .concat()
}

fn outcome_page(success: bool, detail: &str) -> String {
    let (title, heading, copy, mark) = if success {
        (
            "Sign-in confirmed — Aeris",
            "You’re signed in",
            "Authentication is complete. Return to Aeris to continue.",
            r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6.8 12.4 3.2 3.2 7.2-7.2"/></svg>"#.to_string(),
        )
    } else {
        (
            "Sign-in failed — Aeris",
            "Sign-in wasn’t completed",
            "Return to Aeris and try again.",
            "!".to_string(),
        )
    };
    let detail = if success {
        String::new()
    } else {
        format!("<p class=\"detail\">{}</p>", escape_html(detail))
    };
    let state = if success { "success" } else { "failure" };
    let platform = browser_platform_styles();
    let fonts = browser_font_faces();
    let brand_mark = BRAND_MARK;
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>{title}</title>{SYSTEM_THEME_BOOTSTRAP}<style>
{fonts}
{platform}
html,body{{margin:0;min-height:100%}}body{{min-height:100vh;display:grid;place-items:center;background:var(--surface);color:var(--text-default);padding:24px}}main{{width:min(420px,100%);text-align:center}}.brand{{display:flex;align-items:center;justify-content:center;gap:10px;margin-bottom:40px;color:var(--text-default);font-size:17px;font-weight:700;letter-spacing:-.02em}}.brand-mark{{width:40px;height:40px;display:block;flex:none}}.brand-mark>svg{{display:block;width:40px;height:40px}}.mark{{width:64px;height:64px;margin:0 auto 24px;display:grid;place-items:center;border:1px solid var(--border);border-radius:var(--radius-large);background:var(--surface-secondary);font-size:27px;font-weight:700;animation:arrive .34s cubic-bezier(.2,.8,.2,1) both}}.success .mark{{color:var(--primary)}}.failure .mark{{color:var(--danger)}}.mark>svg{{width:30px;height:30px;fill:none;stroke:currentColor;stroke-width:2.2;stroke-linecap:round;stroke-linejoin:round}}.mark>svg path{{stroke-dasharray:18;stroke-dashoffset:18;animation:draw .4s .18s ease-out forwards}}h1{{margin:0 0 9px;font-size:24px;font-weight:700;letter-spacing:-.025em}}p{{margin:0;color:var(--text-secondary);font-size:14px;line-height:1.55}}.detail{{margin:16px auto 0;max-width:360px;color:var(--danger);font-size:13px}}@keyframes arrive{{from{{opacity:0;transform:scale(.82)}}to{{opacity:1;transform:scale(1)}}}}@keyframes draw{{to{{stroke-dashoffset:0}}}}@media(prefers-reduced-motion:reduce){{.mark,.mark>svg path{{animation:none}}.mark>svg path{{stroke-dashoffset:0}}}}
</style></head><body><main class="{state}"><div class="brand"><span class="brand-mark">{brand_mark}</span><span>Aeris</span></div><div class="mark">{mark}</div><h1>{heading}</h1><p>{copy}</p>{detail}</main></body></html>"#
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
    /// OAuth state echoed by the identity provider.
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
    let mut declined = false;
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
            "error" => declined = true,
            _ => {}
        }
    }
    if declined {
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
        LoopbackListener, browser_platform_styles, callback_query_from_request, outcome_page,
        validate_callback_query,
    };
    use std::{
        io::{Read, Write},
        net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
        time::Duration,
    };

    const CALLBACK: &[u8] = b"GET /callback?code=live-code&state=live-state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

    /// OS-assigned ports keep parallel tests from contending for the
    /// registered production ports.
    fn listener() -> LoopbackListener {
        LoopbackListener::bind(&[0]).expect("loopback binds")
    }

    #[test]
    fn callback_path_parses_its_query() {
        let request = "GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
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
    fn redirect_uses_localhost_on_the_bound_port() {
        let listener = listener();
        assert_eq!(
            listener.redirect_uri(),
            format!("http://localhost:{}/callback", listener.port())
        );
    }

    #[test]
    fn busy_registered_ports_fall_through_and_then_fail_actionably() {
        let held = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("hold");
        let busy = held.local_addr().expect("held address").port();
        let error = LoopbackListener::bind(&[busy])
            .err()
            .expect("a busy port cannot be bound");
        assert!(error.contains(&busy.to_string()) && error.contains("in use"));
        let fallback = LoopbackListener::bind(&[busy, 0]).expect("next port binds");
        assert_ne!(fallback.port(), busy);
    }

    /// Drives one browser request and returns the page it read.
    fn browser_request(address: SocketAddr, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(address).expect("loopback connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("client timeout");
        stream.write_all(request).expect("request writes");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("callback response reads");
        String::from_utf8_lossy(&response).into_owned()
    }

    fn ipv4(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    #[test]
    fn bound_listener_holds_the_connection_for_the_outcome() {
        let listener = listener();
        let port = listener.port();
        let sender = std::thread::spawn(move || browser_request(ipv4(port), CALLBACK));
        // Receipt alone answers nothing: the browser waits while the runtime
        // exchanges, verifies, and stores.
        let callback = listener
            .accept_one(Duration::from_secs(10))
            .expect("callback arrives");
        let validated =
            validate_callback_query(callback.query(), "live-state").expect("callback validates");
        assert_eq!(validated.code, "live-code");
        callback.respond_outcome(Ok(()));
        let response = sender.join().expect("sender joins");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(
            response.contains("You’re signed in") && response.contains("@keyframes draw"),
            "success page must reflect completion"
        );
        assert!(response.contains("--surface: #ffffff"));
        assert!(response.contains("--surface: #171717"));
        assert!(response.contains("var(--primary)"));
        let page_rules = response.replace(browser_platform_styles(), "");
        assert!(!page_rules.contains("var(--bullish)"));
        assert!(!response.contains("#090b0f"));
        assert!(response.contains("font-family:\"DM Sans\""));
        for weight in ["400", "500", "700"] {
            assert!(response.contains(&format!("font-weight:{weight};font-display:swap")));
        }
        assert!(response.contains("font-family:\"Faculty Glyphic\""));
        assert!(response.contains("data:font/ttf;base64,"));
        assert!(response.contains("classList.toggle('dark',d)"));
        assert!(response.contains("class=\"brand-mark\"><svg"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8"));
        assert!(response.contains("script-src 'unsafe-inline'"));
        assert!(response.contains("font-src data:"));
    }

    #[test]
    fn ipv6_loopback_callbacks_reach_the_same_transaction() {
        let listener = listener();
        if listener.listeners.len() < 2 {
            // This host has no IPv6 loopback, so `localhost` resolves to IPv4.
            return;
        }
        let address = SocketAddr::from((Ipv6Addr::LOCALHOST, listener.port()));
        let sender = std::thread::spawn(move || browser_request(address, CALLBACK));
        let callback = listener
            .accept_one(Duration::from_secs(10))
            .expect("IPv6 callback arrives");
        callback.respond_outcome(Ok(()));
        assert!(
            sender
                .join()
                .expect("sender joins")
                .starts_with("HTTP/1.1 200 OK")
        );
    }

    #[test]
    fn engine_failure_reaches_the_browser_truthfully() {
        let listener = listener();
        let port = listener.port();
        let sender = std::thread::spawn(move || browser_request(ipv4(port), CALLBACK));
        let callback = listener
            .accept_one(Duration::from_secs(10))
            .expect("callback arrives");
        callback.respond_outcome(Err("sign-in verification failed; retry sign-in"));
        let response = sender.join().expect("sender joins");
        // A received callback alone must never produce "fully
        // authenticated": failures carry the redacted runtime detail.
        assert!(response.starts_with("HTTP/1.1 500"));
        assert!(response.contains("sign-in verification failed; retry sign-in"));
        assert!(!response.contains("You’re signed in"));
    }

    #[test]
    fn stray_requests_are_answered_and_the_callback_still_arrives() {
        let listener = listener();
        let port = listener.port();
        let sender = std::thread::spawn(move || {
            let stray = browser_request(
                ipv4(port),
                b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            );
            (stray, browser_request(ipv4(port), CALLBACK))
        });
        let callback = listener
            .accept_one(Duration::from_secs(10))
            .expect("callback arrives after a stray request");
        callback.respond_outcome(Ok(()));
        let (stray, response) = sender.join().expect("sender joins");
        assert!(stray.starts_with("HTTP/1.1 404"));
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn invalid_callbacks_answer_immediately() {
        let listener = listener();
        let port = listener.port();
        let sender = std::thread::spawn(move || {
            browser_request(
                ipv4(port),
                b"GET /callback?code=live-code&state=wrong-state HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
        });
        let callback = listener
            .accept_one(Duration::from_secs(10))
            .expect("callback arrives");
        assert!(validate_callback_query(callback.query(), "live-state").is_err());
        callback.respond_invalid("authorization state mismatch; retry sign-in");
        let response = sender.join().expect("sender joins");
        assert!(response.starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn cancelled_transactions_stop_waiting() {
        let listener = listener();
        let error = listener
            .accept_one_while(Duration::from_secs(10), || false)
            .err()
            .expect("cancelled transaction stops");
        assert!(error.contains("no longer active"));
    }

    #[test]
    fn failure_page_escapes_detail() {
        let page = outcome_page(false, "failed <script>alert('x')</script>");
        assert!(page.contains("&lt;script&gt;"));
        assert!(!page.contains("<script>alert('x')</script>"));
        assert!(page.contains(":root"));
        assert!(page.contains("@font-face"));
        assert!(page.contains("classList.toggle('dark',d)"));
        assert!(page.contains("class=\"brand-mark\"><svg"));
        assert!(page.contains("var(--danger)"));
        let page_rules = page.replace(browser_platform_styles(), "");
        assert!(!page_rules.contains("var(--bearish)"));
    }
}
