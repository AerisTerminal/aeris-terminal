//! Integration coverage for the bounded PKCE loopback redirect listener.
//!
//! Every case drives a real `127.0.0.1` socket so the accept loop, bounds, deadline, and
//! `state` verification are exercised rather than simulated.

use axiusflow_platform_runtime::{LoopbackCallbackError, LoopbackRedirectListener, PkceSecret};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

const REDIRECT_PATH: &str = "/axiusflow/callback";
const DEADLINE: Duration = Duration::from_secs(5);
const SHORT_DEADLINE: Duration = Duration::from_millis(300);

fn new_secret() -> PkceSecret {
    PkceSecret::generate().expect("the operating-system CSPRNG is available")
}

fn bind() -> (LoopbackRedirectListener, u16) {
    let listener = LoopbackRedirectListener::bind(REDIRECT_PATH).expect("loopback port binds");
    let port = listener
        .local_addr()
        .expect("bound address is queryable")
        .port();
    (listener, port)
}

/// Sends one raw request and returns the response, so the caller can sequence requests.
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

/// Sends a request that the listener may reject before draining it.
///
/// When the listener refuses an oversized or flooded request it answers and closes while
/// unread bytes remain, which makes the kernel reset the connection. A reset is therefore a
/// valid rejection signal, so this helper reports it instead of failing the test.
fn send_rejected_request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("loopback accepts connections");
    if stream.write_all(request.as_bytes()).is_err() {
        return String::from("HTTP/1.1 400 Bad Request (connection reset)");
    }
    if stream.flush().is_err() {
        return String::from("HTTP/1.1 400 Bad Request (connection reset)");
    }
    let mut response = String::new();
    match stream.read_to_string(&mut response) {
        Ok(_) if !response.is_empty() => response,
        _ => String::from("HTTP/1.1 400 Bad Request (connection reset)"),
    }
}

fn verified_request(secret: &PkceSecret, code: &str) -> String {
    format!(
        "GET {REDIRECT_PATH}?code={code}&state={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        secret.state()
    )
}

#[test]
fn matching_state_yields_the_authorization_code_and_confirms_to_the_browser() {
    let (listener, port) = bind();
    let secret = new_secret();
    assert_eq!(
        listener.redirect_uri().expect("redirect uri is derivable"),
        format!("http://127.0.0.1:{port}{REDIRECT_PATH}")
    );

    let request = verified_request(&secret, "authorization-code-value");
    let client = thread::spawn(move || send_request(port, &request));

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
    let (listener, port) = bind();
    let secret = new_secret();
    let request = verified_request(&secret, "super-secret-code");
    let client = thread::spawn(move || send_request(port, &request));
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

/// A forged `state` must be answered with `400` yet must never abort the pending sign-in.
#[test]
fn forged_state_cannot_abort_the_pending_sign_in() {
    let (listener, port) = bind();
    let secret = new_secret();
    let verified = verified_request(&secret, "real-code");
    let client = thread::spawn(move || {
        let forged = send_request(
            port,
            &format!("GET {REDIRECT_PATH}?code=attacker-code&state=forged-state HTTP/1.1\r\n\r\n"),
        );
        assert!(forged.starts_with("HTTP/1.1 400 Bad Request"), "{forged}");
        send_request(port, &verified)
    });

    let code = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect("the later verified redirect still resolves the sign-in");
    assert_eq!(code.value(), "real-code");

    let response = client.join().expect("the client thread completes");
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
}

/// Stray local traffic must be rejected individually without denying the sign-in.
#[test]
fn stray_local_requests_cannot_deny_the_sign_in() {
    let (listener, port) = bind();
    let secret = new_secret();
    let verified = verified_request(&secret, "resilient-code");
    let state = secret.state().to_string();

    let client = thread::spawn(move || {
        let mut flood = format!("GET {REDIRECT_PATH}?code=a&state={state} HTTP/1.1\r\n");
        for index in 0..256 {
            let _ = write!(flood, "X-Axiusflow-{index}: padding\r\n");
        }
        flood.push_str("\r\n");

        for stray in [
            format!("GET /unexpected?code=a&state={state} HTTP/1.1\r\n\r\n"),
            format!("POST {REDIRECT_PATH} HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
            format!(
                "GET {REDIRECT_PATH}?code={} HTTP/1.1\r\n\r\n",
                "a".repeat(16_384)
            ),
            flood,
        ] {
            let response = send_rejected_request(port, &stray);
            assert!(
                response.starts_with("HTTP/1.1 400 Bad Request"),
                "stray request should be refused: {response}"
            );
        }
        send_request(port, &verified)
    });

    let code = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect("stray traffic must not deny the verified redirect");
    assert_eq!(code.value(), "resilient-code");
    let _ = client.join();
}

/// The advertised deadline must actually bound the wait when no redirect arrives.
#[test]
fn deadline_is_enforced_when_no_redirect_arrives() {
    let (listener, _port) = bind();
    let secret = new_secret();

    let started = Instant::now();
    let error = listener
        .accept_authorization_code(&secret, SHORT_DEADLINE)
        .expect_err("an absent redirect must not block forever");
    let elapsed = started.elapsed();

    assert!(
        matches!(error, LoopbackCallbackError::DeadlineExceeded),
        "unexpected error: {error:?}"
    );
    assert!(
        elapsed >= SHORT_DEADLINE,
        "returned before the deadline elapsed: {elapsed:?}"
    );
    assert!(
        elapsed < SHORT_DEADLINE * 20,
        "waited far longer than the deadline: {elapsed:?}"
    );
}

/// An unverified `error` response must not abort the flow, unlike a verified one.
#[test]
fn authorization_server_error_requires_a_verified_state() {
    let (listener, port) = bind();
    let secret = new_secret();
    let state = secret.state().to_string();
    let client = thread::spawn(move || {
        let unverified = send_request(
            port,
            &format!("GET {REDIRECT_PATH}?error=access_denied&state=forged HTTP/1.1\r\n\r\n"),
        );
        assert!(
            unverified.starts_with("HTTP/1.1 400 Bad Request"),
            "{unverified}"
        );
        send_request(
            port,
            &format!("GET {REDIRECT_PATH}?error=access%5Fdenied&state={state} HTTP/1.1\r\n\r\n"),
        )
    });

    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a verified authorization-server error aborts the flow");
    let _ = client.join();
    match error {
        LoopbackCallbackError::AuthorizationServerError(reported) => {
            assert_eq!(reported, "access_denied");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

/// Reported authorization-server errors must be reduced to bounded RFC 6749 codes.
#[test]
fn reported_authorization_server_error_is_sanitized_and_bounded() {
    let (listener, port) = bind();
    let secret = new_secret();
    let state = secret.state().to_string();
    let client = thread::spawn(move || {
        send_request(
            port,
            &format!(
                "GET {REDIRECT_PATH}?error=access_denied%20%3Cinjected%20log%20line%3E%0A{}&state={state} HTTP/1.1\r\n\r\n",
                "z".repeat(256)
            ),
        )
    });

    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a verified authorization-server error aborts the flow");
    let _ = client.join();
    match error {
        LoopbackCallbackError::AuthorizationServerError(reported) => {
            assert!(
                reported.starts_with("access_denied"),
                "unexpected report: {reported}"
            );
            assert!(reported.len() <= 64, "report was not bounded: {reported}");
            assert!(
                reported
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '_' | '-')),
                "report retained unsafe characters: {reported}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

/// A verified redirect that carries no code is a real failure and must abort.
#[test]
fn verified_redirect_without_a_code_is_rejected() {
    let (listener, port) = bind();
    let secret = new_secret();
    let state = secret.state().to_string();
    let client = thread::spawn(move || {
        send_request(
            port,
            &format!("GET {REDIRECT_PATH}?state={state} HTTP/1.1\r\n\r\n"),
        )
    });

    let error = listener
        .accept_authorization_code(&secret, DEADLINE)
        .expect_err("a verified redirect without a code must be rejected");
    let _ = client.join();
    assert!(
        matches!(error, LoopbackCallbackError::MissingAuthorizationCode),
        "unexpected error: {error:?}"
    );
}
