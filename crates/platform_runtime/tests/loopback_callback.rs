//! Integration coverage for the bounded PKCE loopback redirect listener.
//!
//! Every case drives a real `127.0.0.1` socket so the accept, bound, and verification
//! behavior is exercised rather than simulated.

use axiusflow_platform_runtime::{LoopbackCallbackError, LoopbackRedirectListener, PkceSecret};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

const REDIRECT_PATH: &str = "/axiusflow/callback";
const DEADLINE: Duration = Duration::from_secs(5);

fn new_secret() -> PkceSecret {
    PkceSecret::generate().expect("the operating-system CSPRNG is available")
}

/// Sends one raw request to the listener and returns its response body.
fn send_request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("loopback accepts connections");
    stream
        .write_all(request.as_bytes())
        .expect("request is written");
    stream.flush().expect("request is flushed");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response is readable");
    response
}

fn spawn_request(port: u16, request: String) -> thread::JoinHandle<String> {
    thread::spawn(move || send_request(port, &request))
}

#[test]
fn matching_state_yields_the_authorization_code_and_confirms_to_the_browser() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();

    let redirect_uri = listener.redirect_uri().expect("redirect uri is derivable");
    assert_eq!(
        redirect_uri,
        format!("http://127.0.0.1:{port}{REDIRECT_PATH}")
    );

    let request = format!(
        "GET {REDIRECT_PATH}?code=authorization-code-value&state={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        secret.state()
    );
    let client = spawn_request(port, request);

    let code = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect("a verified redirect yields the authorization code");
    assert_eq!(code.value(), "authorization-code-value");

    let response = client.join().expect("the client thread completes");
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("Cache-Control: no-store"), "{response}");
    assert!(
        !response.contains("authorization-code-value"),
        "the browser response must never echo the authorization code"
    );
}

#[test]
fn authorization_code_debug_output_redacts_the_code() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let request = format!(
        "GET {REDIRECT_PATH}?code=super-secret-code&state={} HTTP/1.1\r\n\r\n",
        secret.state()
    );
    let client = spawn_request(port, request);
    let code = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect("a verified redirect yields the authorization code");
    let _ = client.join();

    let rendered = format!("{code:?}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(
        !rendered.contains("super-secret-code"),
        "the authorization code must never appear in debug output"
    );
}

#[test]
fn mismatched_state_is_rejected_before_any_code_is_returned() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let request =
        format!("GET {REDIRECT_PATH}?code=attacker-code&state=forged-state HTTP/1.1\r\n\r\n");
    let client = spawn_request(port, request);

    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a forged state must be rejected");
    assert!(
        matches!(error, LoopbackCallbackError::StateMismatch),
        "unexpected error: {error:?}"
    );

    let response = client.join().expect("the client thread completes");
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "{response}"
    );
}

#[test]
fn missing_state_and_missing_code_are_each_rejected() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let client = spawn_request(
        port,
        format!("GET {REDIRECT_PATH}?code=orphan-code HTTP/1.1\r\n\r\n"),
    );
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a redirect without state must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::MissingState),
        "unexpected error: {error:?}"
    );

    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let client = spawn_request(
        port,
        format!(
            "GET {REDIRECT_PATH}?state={} HTTP/1.1\r\n\r\n",
            secret.state()
        ),
    );
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a redirect without a code must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::MissingAuthorizationCode),
        "unexpected error: {error:?}"
    );
}

#[test]
fn authorization_server_error_is_surfaced_and_percent_decoded() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let client = spawn_request(
        port,
        format!(
            "GET {REDIRECT_PATH}?error=access%5Fdenied&state={} HTTP/1.1\r\n\r\n",
            secret.state()
        ),
    );
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("an authorization-server error must be surfaced");
    let _ = client.join();
    match error {
        LoopbackCallbackError::AuthorizationServerError(reported) => {
            assert_eq!(reported, "access_denied");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn wrong_path_and_wrong_method_are_rejected() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let client = spawn_request(
        port,
        "GET /unexpected?code=a&state=b HTTP/1.1\r\n\r\n".to_string(),
    );
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a foreign path must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::RedirectPathMismatch),
        "unexpected error: {error:?}"
    );

    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let client = spawn_request(
        port,
        format!("POST {REDIRECT_PATH} HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
    );
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a non-GET method must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::UnsupportedMethod),
        "unexpected error: {error:?}"
    );
}

#[test]
fn oversized_request_line_and_header_flood_are_bounded() {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let oversized = "a".repeat(16_384);
    let client = spawn_request(
        port,
        format!("GET {REDIRECT_PATH}?code={oversized} HTTP/1.1\r\n\r\n"),
    );
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("an oversized request line must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::RequestLineTooLong),
        "unexpected error: {error:?}"
    );

    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    let secret = new_secret();
    let mut flood = format!(
        "GET {REDIRECT_PATH}?code=a&state={} HTTP/1.1\r\n",
        secret.state()
    );
    for index in 0..256 {
        let _ = write!(flood, "X-Axiusflow-{index}: padding\r\n");
    }
    flood.push_str("\r\n");
    let client = spawn_request(port, flood);
    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a header flood must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::TooManyHeaders),
        "unexpected error: {error:?}"
    );
}
