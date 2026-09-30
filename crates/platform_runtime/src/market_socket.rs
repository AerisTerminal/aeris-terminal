//! Blocking WebSocket transport shared by market-data provider adapters.
//!
//! Exactly one market-runtime-owned thread drives one of these sockets at a
//! time: connect with bounded TCP/TLS setup, exchange bounded text frames, and
//! shut the socket down from another thread for prompt cancellation. Only
//! text frames are accepted; provider feeds are text JSON, so a binary frame
//! fails the connection rather than entering the decoders.

use std::{
    fmt,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tungstenite::{
    Connector, Message, WebSocket, protocol::WebSocketConfig, stream::MaybeTlsStream,
};

/// Largest single WebSocket message accepted from a provider feed.
pub const MAXIMUM_MARKET_SOCKET_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
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

/// Redacted socket failure. Only [`MarketSocketError::TimedOut`] means the
/// connection is healthy but quiet; every other variant means the owning
/// worker must reconnect or stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketSocketError {
    InvalidUrl,
    DeadlineOverflow,
    Cancelled,
    Resolution,
    Connect,
    Setup,
    Tls,
    Handshake,
    Send,
    TimedOut,
    Read,
    Binary,
    Pong,
    Closed,
}

impl MarketSocketError {
    /// Whether a read only lapsed its deadline on a healthy, quiet socket.
    #[must_use]
    pub const fn is_read_timeout(self) -> bool {
        matches!(self, Self::TimedOut)
    }
}

impl fmt::Display for MarketSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => "market socket URL is invalid",
            Self::DeadlineOverflow => "market socket deadline overflowed",
            Self::Cancelled => "market socket cancelled",
            Self::Resolution => "market socket resolution failed",
            Self::Connect => "market socket connect failed",
            Self::Setup => "market socket setup failed",
            Self::Tls => "market socket TLS setup failed",
            Self::Handshake => "market socket handshake failed",
            Self::Send => "market socket send failed",
            Self::TimedOut => "market socket read timed out",
            Self::Read => "market socket read failed",
            Self::Binary => "market socket sent binary",
            Self::Pong => "market socket pong failed",
            Self::Closed => "market socket closed",
        })
    }
}

impl std::error::Error for MarketSocketError {}

/// One blocking market-feed connection with an explicit operation deadline.
pub struct MarketSocket {
    socket: WebSocket<MaybeTlsStream<DeadlineTcpStream>>,
    shutdown: MarketSocketShutdown,
}

/// Closes the underlying TCP stream from any thread to unblock a pending
/// socket read or write without waiting out its deadline.
#[derive(Clone)]
pub struct MarketSocketShutdown {
    stream: Arc<TcpStream>,
}

impl MarketSocketShutdown {
    /// Unblocks a pending socket operation; the owning worker observes its
    /// stop flag next and exits.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// One event read from the market feed.
#[derive(Debug, PartialEq, Eq)]
pub enum MarketSocketEvent {
    /// One complete text message, still raw for the fixed-point decoders.
    Text(String),
    /// A heartbeat reply; the worker measures liveness from these.
    Pong,
}

impl MarketSocket {
    /// Opens the feed socket with a bounded TCP/TLS handshake.
    ///
    /// DNS uses the shared bounded platform resolver; the overall deadline
    /// covers resolution, TCP attempts, and the TLS/WebSocket handshake.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid URL, failed resolution or TCP/TLS
    /// setup, a failed handshake, or a cancelled stop flag.
    pub fn connect(
        url: &str,
        timeout: Duration,
        stop: &Arc<AtomicBool>,
    ) -> Result<(Self, MarketSocketShutdown), MarketSocketError> {
        let (host, port) = split_host_port(url)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(MarketSocketError::DeadlineOverflow)?;
        let stream = connect_tcp(&host, port, deadline, stop)?;
        stream
            .set_nodelay(true)
            .map_err(|_| MarketSocketError::Setup)?;
        let shutdown = MarketSocketShutdown {
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
            .max_message_size(Some(MAXIMUM_MARKET_SOCKET_MESSAGE_BYTES))
            .max_frame_size(Some(MAXIMUM_MARKET_SOCKET_MESSAGE_BYTES));
        let (socket, _) = tungstenite::client_tls_with_config(
            url,
            tcp,
            Some(websocket_config),
            Some(Connector::Rustls(Arc::new(tls_config()?))),
        )
        .map_err(|_| MarketSocketError::Handshake)?;
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
    pub fn shutdown_handle(&self) -> MarketSocketShutdown {
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
    pub fn send_text(&mut self, text: &str) -> Result<(), MarketSocketError> {
        self.set_write_deadline(write_deadline());
        self.socket
            .send(Message::Text(text.to_owned().into()))
            .map_err(|_| MarketSocketError::Send)
    }

    /// Reads one feed event, waiting at most until `deadline`.
    ///
    /// Ping frames are answered in place and never surface; only text and
    /// pong reach the caller. A quiet socket fails with
    /// [`MarketSocketError::TimedOut`] (the worker heartbeats and waits
    /// again); every other failure means the connection is broken and the
    /// worker must reconnect.
    ///
    /// # Errors
    ///
    /// Returns an error on close frames, binary frames, oversized messages,
    /// I/O failure, deadline expiry, or cancellation.
    pub fn read_event(
        &mut self,
        deadline: Instant,
    ) -> Result<MarketSocketEvent, MarketSocketError> {
        self.set_read_deadline(deadline);
        loop {
            let message = self.socket.read().map_err(map_read_error)?;
            match message {
                Message::Text(text) => {
                    return Ok(MarketSocketEvent::Text(text.as_str().to_owned()));
                }
                Message::Binary(_) => return Err(MarketSocketError::Binary),
                Message::Ping(_) => {
                    // Tungstenite queues the matching pong automatically. Its
                    // flush needs a fresh write deadline even when the socket
                    // has been reading since the last application ping.
                    self.set_write_deadline(write_deadline());
                    self.socket.flush().map_err(|_| MarketSocketError::Pong)?;
                }
                Message::Pong(_) => return Ok(MarketSocketEvent::Pong),
                Message::Close(_) => return Err(MarketSocketError::Closed),
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

    fn set_write_deadline(&mut self, deadline: Instant) {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_write_deadline(deadline),
            MaybeTlsStream::Rustls(stream) => stream.get_mut().set_write_deadline(deadline),
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

fn map_read_error(error: tungstenite::Error) -> MarketSocketError {
    match error {
        tungstenite::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            MarketSocketError::TimedOut
        }
        tungstenite::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            MarketSocketError::Cancelled
        }
        _ => MarketSocketError::Read,
    }
}

fn tls_config() -> Result<rustls::ClientConfig, MarketSocketError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
    .map_err(|_| MarketSocketError::Tls)
}

fn split_host_port(url: &str) -> Result<(String, u16), MarketSocketError> {
    let invalid = || MarketSocketError::InvalidUrl;
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
    // Strip optional userinfo; provider feeds authenticate in-band, never in the URL.
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
) -> Result<Arc<TcpStream>, MarketSocketError> {
    if stop.load(Ordering::Acquire) {
        return Err(MarketSocketError::Cancelled);
    }
    let addresses = crate::resolve_addresses(host, port, deadline, Some(stop))
        .map_err(|_| MarketSocketError::Resolution)?;
    if addresses.is_empty() {
        return Err(MarketSocketError::Resolution);
    }
    for address in addresses {
        if stop.load(Ordering::Acquire) {
            return Err(MarketSocketError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining.min(CONNECT_ATTEMPT_TIMEOUT)) {
            Ok(stream) => return Ok(Arc::new(stream)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Err(MarketSocketError::Cancelled);
            }
            Err(_) => {}
        }
    }
    Err(MarketSocketError::Connect)
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
    fn server_ping_uses_fresh_write_deadline_and_keeps_session_open() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
        let address = listener.local_addr().expect("loopback address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("client connects");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("server read timeout");
            let mut socket = tungstenite::accept(stream).expect("websocket handshake");
            socket
                .send(Message::Ping(vec![1, 2, 3].into()))
                .expect("server sends ping");
            assert_eq!(
                socket.read().expect("client answers ping"),
                Message::Pong(vec![1, 2, 3].into())
            );
            socket
                .send(Message::Text("still connected".into()))
                .expect("server sends data after pong");
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (mut socket, _) =
            MarketSocket::connect(&format!("ws://{address}/"), Duration::from_secs(3), &stop)
                .expect("client connects");
        socket.set_write_deadline(past());
        assert_eq!(
            socket
                .read_event(Instant::now() + Duration::from_secs(3))
                .expect("ping does not disconnect the client"),
            MarketSocketEvent::Text("still connected".to_string())
        );
        server.join().expect("server finishes");
    }

    #[test]
    fn quiet_socket_reports_only_a_read_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
        let address = listener.local_addr().expect("loopback address");
        let (release, wait) = std::sync::mpsc::sync_channel::<()>(1);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("client connects");
            let _socket = tungstenite::accept(stream).expect("websocket handshake");
            let _ = wait.recv_timeout(Duration::from_secs(3));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (mut socket, _) =
            MarketSocket::connect(&format!("ws://{address}/"), Duration::from_secs(3), &stop)
                .expect("client connects");
        let error = socket
            .read_event(Instant::now() + Duration::from_millis(50))
            .expect_err("quiet socket times out");
        assert!(error.is_read_timeout());
        release.send(()).expect("server released");
        server.join().expect("server finishes");
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
    fn socket_urls_require_websocket_schemes_and_hosts() {
        assert_eq!(
            split_host_port("wss://example.test/realtime"),
            Ok(("example.test".to_string(), 443))
        );
        assert_eq!(
            split_host_port("ws://127.0.0.1:9000/"),
            Ok(("127.0.0.1".to_string(), 9000))
        );
        assert_eq!(
            split_host_port("https://example.test"),
            Err(MarketSocketError::InvalidUrl)
        );
        assert_eq!(
            split_host_port("wss://:443/"),
            Err(MarketSocketError::InvalidUrl)
        );
    }
}
