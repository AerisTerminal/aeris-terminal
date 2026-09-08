//! Blocking WebSocket transport for the public Hyperliquid market feed.
//!
//! Exactly one engine-owned thread drives one of these sockets at a time:
//! connect with bounded TCP/TLS setup, exchange bounded text frames, and
//! shut the socket down from another thread for prompt cancellation. Only
//! text frames are accepted; the public feed is text JSON, so a binary frame
//! fails the connection rather than entering the decoders.

use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tungstenite::{
    Connector, Message, WebSocket, protocol::WebSocketConfig, stream::MaybeTlsStream,
};

/// Largest single WebSocket message accepted from the public feed.
pub const MAXIMUM_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
/// How long one socket read or write waits before the worker re-checks its
/// controls, heartbeat, and cancellation flag.
const NETWORK_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound for one TCP connect attempt against a resolved address.
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for one socket write (subscription, ping, pong, close).
///
/// Writes carry their own deadline because the read deadline routinely lapses
/// on a quiet feed: reusing it would fail the next heartbeat instantly and
/// force a spurious reconnect right when the feed is merely idle.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// One blocking market-feed connection with an explicit operation deadline.
pub struct HyperliquidSocket {
    socket: WebSocket<MaybeTlsStream<DeadlineTcpStream>>,
    shutdown: HyperliquidSocketShutdown,
}

/// Closes the underlying TCP stream from any thread to unblock a pending
/// socket read or write without waiting out its deadline.
#[derive(Clone)]
pub struct HyperliquidSocketShutdown {
    stream: Arc<TcpStream>,
}

impl HyperliquidSocketShutdown {
    /// Unblocks a pending socket operation; the owning worker observes its
    /// stop flag next and exits.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// One event read from the market feed.
#[derive(Debug, PartialEq, Eq)]
pub enum SocketEvent {
    /// One complete text message, still raw for the fixed-point decoders.
    Text(String),
    /// A heartbeat reply; the worker measures liveness from these.
    Pong,
}

impl HyperliquidSocket {
    /// Opens the feed socket with a bounded TCP/TLS handshake.
    ///
    /// DNS resolution runs on the calling (provider-owned) thread, matching
    /// the blocking HTTP history path; every TCP attempt carries its own
    /// timeout and the overall `timeout` bounds the handshake.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid URL, failed resolution or TCP/TLS
    /// setup, a failed handshake, or a cancelled stop flag.
    pub fn connect(
        url: &str,
        timeout: Duration,
        stop: &Arc<AtomicBool>,
    ) -> Result<(Self, HyperliquidSocketShutdown), String> {
        let (host, port) = split_host_port(url)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "hyperliquid socket deadline overflowed".to_string())?;
        let stream = connect_tcp(&host, port, deadline, stop)?;
        stream
            .set_nodelay(true)
            .map_err(|_| "hyperliquid socket setup failed".to_string())?;
        let shutdown = HyperliquidSocketShutdown {
            stream: Arc::clone(&stream),
        };
        let tcp = DeadlineTcpStream {
            stream,
            stop: Arc::clone(stop),
            read_deadline: deadline,
            write_deadline: deadline,
        };
        let websocket_config = WebSocketConfig::default()
            .read_buffer_size(64 * 1024)
            .max_message_size(Some(MAXIMUM_MESSAGE_BYTES))
            .max_frame_size(Some(MAXIMUM_MESSAGE_BYTES));
        let (socket, _) = tungstenite::client_tls_with_config(
            url,
            tcp,
            Some(websocket_config),
            Some(Connector::Rustls(Arc::new(tls_config()?))),
        )
        .map_err(|_| "hyperliquid socket handshake failed".to_string())?;
        Ok((
            Self {
                socket,
                shutdown: shutdown.clone(),
            },
            shutdown,
        ))
    }

    /// Returns the handle that unblocks a pending socket operation.
    #[must_use]
    pub fn shutdown_handle(&self) -> HyperliquidSocketShutdown {
        self.shutdown.clone()
    }

    /// Sends one complete text message with a fresh write deadline.
    ///
    /// The write deadline is independent of the read deadline on purpose: a
    /// heartbeat sent after a read timeout must still get its full write
    /// window, otherwise every quiet stretch ends in a spurious reconnect.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket write fails or is cancelled.
    pub fn send_text(&mut self, text: &str) -> Result<(), String> {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_write_deadline(write_deadline()),
            MaybeTlsStream::Rustls(stream) => stream.get_mut().set_write_deadline(write_deadline()),
            _ => {}
        }
        self.socket
            .send(Message::Text(text.to_owned().into()))
            .map_err(|_| "hyperliquid socket send failed".to_string())
    }

    /// Reads one feed event, waiting at most until `deadline`.
    ///
    /// Ping frames are answered in place and never surface; only text and
    /// pong reach the caller. A quiet socket fails with the timeout error
    /// recognized by [`is_read_timeout`] (the worker heartbeats and waits
    /// again); every other failure means the connection is broken and the
    /// worker must reconnect.
    ///
    /// # Errors
    ///
    /// Returns an error on close frames, binary frames, oversized messages,
    /// I/O failure, deadline expiry, or cancellation.
    pub fn read_event(&mut self, deadline: Instant) -> Result<SocketEvent, String> {
        self.set_read_deadline(deadline);
        loop {
            let message = self.socket.read().map_err(map_read_error)?;
            match message {
                Message::Text(text) => return Ok(SocketEvent::Text(text.as_str().to_owned())),
                Message::Binary(_) => return Err("hyperliquid socket sent binary".to_string()),
                Message::Ping(payload) => {
                    self.socket
                        .send(Message::Pong(payload))
                        .map_err(|_| "hyperliquid socket send failed".to_string())?;
                }
                Message::Pong(_) => return Ok(SocketEvent::Pong),
                Message::Close(_) => return Err("hyperliquid socket closed".to_string()),
                Message::Frame(_) => {}
            }
        }
    }

    /// Interrupts the underlying connection without waiting for a peer close.
    ///
    /// The worker uses this only when parking or shutting down. A WebSocket
    /// close handshake can block behind TLS/network state, so lifecycle
    /// cancellation must use the owned TCP shutdown handle instead.
    pub fn close(&mut self) {
        self.shutdown.shutdown();
    }

    fn set_read_deadline(&mut self, deadline: Instant) {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_read_deadline(deadline),
            MaybeTlsStream::Rustls(stream) => stream.get_mut().set_read_deadline(deadline),
            _ => {}
        }
    }
}

/// Fresh deadline for one socket write, independent of any lapsed read.
fn write_deadline() -> Instant {
    Instant::now()
        .checked_add(WRITE_TIMEOUT)
        .unwrap_or_else(Instant::now)
}

fn map_read_error(error: tungstenite::Error) -> String {
    match error {
        tungstenite::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            "hyperliquid socket read timed out".to_string()
        }
        tungstenite::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            "hyperliquid socket cancelled".to_string()
        }
        _ => "hyperliquid socket read failed".to_string(),
    }
}

/// Distinguishes a quiet socket (caller retries with a fresh deadline) from
/// a broken one (caller reconnects). Only the timeout spellings land here;
/// every other failure already returned `Err` above.
#[must_use]
pub fn is_read_timeout(error: &str) -> bool {
    error == "hyperliquid socket read timed out"
}

fn tls_config() -> Result<rustls::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
    .map_err(|_| "hyperliquid TLS setup failed".to_string())
}

fn split_host_port(url: &str) -> Result<(String, u16), String> {
    let invalid = || "hyperliquid socket URL is invalid".to_string();
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    let default_port = match scheme {
        "wss" => 443,
        "ws" => 80,
        _ => return Err(invalid()),
    };
    let authority = rest.split('/').next().ok_or_else(invalid)?;
    if authority.is_empty() {
        return Err(invalid());
    }
    // Strip optional userinfo; the public feed never authenticates.
    let authority = authority.rsplit('@').next().ok_or_else(invalid)?;
    if let Some((host, port)) = authority.rsplit_once(':') {
        let port: u16 = port.parse().map_err(|_| invalid())?;
        if host.is_empty() {
            return Err(invalid());
        }
        Ok((host.to_string(), port))
    } else {
        Ok((authority.to_string(), default_port))
    }
}

fn connect_tcp(
    host: &str,
    port: u16,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<Arc<TcpStream>, String> {
    if stop.load(Ordering::Acquire) {
        return Err("hyperliquid socket cancelled".to_string());
    }
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|_| "hyperliquid socket resolution failed".to_string())?
        .take(16)
        .collect();
    if addresses.is_empty() {
        return Err("hyperliquid socket resolution failed".to_string());
    }
    for address in addresses {
        if stop.load(Ordering::Acquire) {
            return Err("hyperliquid socket cancelled".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining.min(CONNECT_ATTEMPT_TIMEOUT)) {
            Ok(stream) => return Ok(Arc::new(stream)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Err("hyperliquid socket cancelled".to_string());
            }
            Err(_) => {}
        }
    }
    Err("hyperliquid socket connect failed".to_string())
}

/// Blocking TCP stream with independent read and write operation deadlines.
struct DeadlineTcpStream {
    stream: Arc<TcpStream>,
    stop: Arc<AtomicBool>,
    read_deadline: Instant,
    write_deadline: Instant,
}

impl DeadlineTcpStream {
    const fn set_read_deadline(&mut self, deadline: Instant) {
        self.read_deadline = deadline;
    }

    const fn set_write_deadline(&mut self, deadline: Instant) {
        self.write_deadline = deadline;
    }

    fn operation_timeout(&self, deadline: Instant) -> std::io::Result<Duration> {
        if self.stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                // rustls deliberately retries `Interrupted`; cancellation
                // must use a terminal kind to escape that internal loop.
                std::io::ErrorKind::ConnectionAborted,
                "cancelled",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "deadline",
            ));
        }
        Ok(remaining.min(NETWORK_POLL_INTERVAL))
    }
}

impl Read for DeadlineTcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            self.stream
                .set_read_timeout(Some(self.operation_timeout(self.read_deadline)?))?;
            match (&*self.stream).read(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

impl Write for DeadlineTcpStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        loop {
            self.stream
                .set_write_timeout(Some(self.operation_timeout(self.write_deadline)?))?;
            match (&*self.stream).write(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        loop {
            self.stream
                .set_write_timeout(Some(self.operation_timeout(self.write_deadline)?))?;
            match (&*self.stream).flush() {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Instant safely in the past for lapsed-deadline cases.
    fn past() -> Instant {
        Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("past instant fits")
    }

    /// Connected loopback pair: hermetic, no external network.
    fn loopback() -> (DeadlineTcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
        let peer = TcpStream::connect(listener.local_addr().expect("loopback addr"))
            .expect("loopback connects");
        let (server, _) = listener.accept().expect("loopback accepts");
        peer.set_nodelay(true).expect("nodelay");
        let stream = DeadlineTcpStream {
            stream: Arc::new(peer),
            stop: Arc::new(AtomicBool::new(false)),
            read_deadline: Instant::now() + Duration::from_secs(60),
            write_deadline: Instant::now() + Duration::from_secs(60),
        };
        (stream, server)
    }

    #[test]
    fn write_deadline_survives_an_expired_read_deadline() {
        let (mut stream, _server) = loopback();
        // A quiet feed lapses the read deadline; the next heartbeat must
        // still get its full write window instead of failing instantly.
        stream.set_read_deadline(past());
        stream.set_write_deadline(write_deadline());
        assert!(stream.operation_timeout(stream.read_deadline).is_err());
        stream
            .write_all(b"ping")
            .expect("write succeeds on a fresh write deadline");
    }

    #[test]
    fn expired_write_deadline_fails_fast_without_touching_read() {
        let (mut stream, _server) = loopback();
        stream.set_write_deadline(past());
        let error = stream
            .operation_timeout(stream.write_deadline)
            .expect_err("lapsed write deadline fails");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(stream.operation_timeout(stream.read_deadline).is_ok());
    }

    #[test]
    fn cancelled_operations_report_terminal_cancellation() {
        let (stream, _server) = loopback();
        stream.stop.store(true, Ordering::Release);
        let error = stream
            .operation_timeout(stream.write_deadline)
            .expect_err("cancelled write reports");
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    }
    #[test]
    #[ignore = "drives the live Hyperliquid public WebSocket"]
    fn live_public_socket_connects_and_receives_book() {
        let stop = Arc::new(AtomicBool::new(false));
        let (mut socket, _) =
            HyperliquidSocket::connect(crate::HYPERLIQUID_WS_URL, Duration::from_secs(10), &stop)
                .expect("live Hyperliquid socket connects");
        socket
            .send_text(&crate::build_l2_subscription("BTC").expect("subscription encodes"))
            .expect("book subscription sends");
        socket
            .send_text(&crate::build_bbo_subscription("BTC").expect("subscription encodes"))
            .expect("BBO subscription sends");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut books = 0;
        let mut quotes = 0;
        let mut sequence = 1;
        while books < 3 || quotes < 3 {
            assert!(Instant::now() < deadline, "live Hyperliquid book timed out");
            match socket.read_event(Instant::now() + Duration::from_secs(5)) {
                Ok(SocketEvent::Text(text)) => {
                    match crate::parse_ws_frame(&text).expect("provider frame") {
                        crate::WsClientEvent::Book { coin, book } => {
                            let decoded = crate::decode_book_snapshot(
                                &book,
                                &coin,
                                "hyperliquid:perp:BTC",
                                "hyperliquid:public",
                                1,
                                sequence,
                                1,
                            )
                            .expect("valid live depth");
                            assert_eq!(decoded.snapshot.bids.len(), 5);
                            assert_eq!(decoded.snapshot.asks.len(), 5);
                            books += 1;
                        }
                        crate::WsClientEvent::Bbo { coin, bbo } => {
                            crate::decode_bbo_quote(
                                &bbo,
                                &coin,
                                "hyperliquid:perp:BTC",
                                "hyperliquid:public",
                                1,
                                sequence,
                                1,
                            )
                            .expect("valid live quote");
                            quotes += 1;
                        }
                        _ => {}
                    }
                    sequence += 1;
                }
                Ok(_) => {}
                Err(error) if is_read_timeout(&error) => {}
                Err(error) => panic!("live Hyperliquid socket failed: {error}"),
            }
        }
        eprintln!("received {books} fast five-level books and {quotes} validated BBO updates");
    }
}
