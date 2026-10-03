use crate::{RithmicSessionError, RithmicSessionLimits, endpoint::RithmicEndpoint};
use aeris_platform_runtime::cancel_tcp_stream_io;
use rustls::{ClientConfig, RootCertStore};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Connector, WebSocket, protocol::WebSocketConfig, stream::MaybeTlsStream};

const NETWORK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TCP_CONNECT_ATTEMPT_LIMIT: Duration = Duration::from_secs(1);
pub(crate) type RithmicWebSocket = WebSocket<MaybeTlsStream<DeadlineTcpStream>>;

/// Marker payload for socket I/O refused because the session owner requested a stop.
#[derive(Debug)]
struct StopRequested;

impl fmt::Display for StopRequested {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Rithmic session stop requested")
    }
}

impl Error for StopRequested {}

/// Whether an I/O failure came from a requested stop rather than the network.
pub(crate) fn is_cancellation(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
        || error
            .get_ref()
            .is_some_and(<dyn Error + Send + Sync>::is::<StopRequested>)
}

#[derive(Debug, Default)]
pub(crate) struct ConnectionAbort {
    aborted: AtomicBool,
    stream: Mutex<Option<Arc<TcpStream>>>,
}

impl ConnectionAbort {
    pub(crate) fn register(&self, stream: Arc<TcpStream>) -> Result<(), RithmicSessionError> {
        if self.aborted.load(Ordering::Acquire) {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(RithmicSessionError::Cancelled);
        }
        let mut registered = self
            .stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.aborted.load(Ordering::Acquire) {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(RithmicSessionError::Cancelled);
        }
        *registered = Some(stream);
        Ok(())
    }

    pub(crate) fn abort(&self) {
        self.aborted.store(true, Ordering::Release);
        if let Some(stream) = self
            .stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = cancel_tcp_stream_io(&stream);
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

pub(crate) fn default_tls_config() -> Result<ClientConfig, RithmicSessionError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| RithmicSessionError::Tls)
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

pub(crate) fn connect_websocket(
    endpoint: RithmicEndpoint,
    limits: RithmicSessionLimits,
    stop: Option<Arc<AtomicBool>>,
    abort: Option<&ConnectionAbort>,
    tls_config: ClientConfig,
) -> Result<RithmicWebSocket, RithmicSessionError> {
    let connect_deadline = Instant::now() + limits.connect_timeout;
    let mut tcp = connect_tcp(endpoint.host, endpoint.port, connect_deadline, stop)?;
    if let Some(abort) = abort {
        abort.register(Arc::clone(&tcp.stream))?;
    }
    tcp.set_deadline(Instant::now() + limits.handshake_timeout);
    let websocket_config = WebSocketConfig::default()
        .read_buffer_size(limits.maximum_message_bytes.min(64 * 1024))
        .write_buffer_size(0)
        .max_write_buffer_size(limits.maximum_write_buffer_bytes)
        .max_message_size(Some(limits.maximum_message_bytes))
        .max_frame_size(Some(limits.maximum_message_bytes));
    tungstenite::client_tls_with_config(
        endpoint.url,
        tcp,
        Some(websocket_config),
        Some(Connector::Rustls(Arc::new(tls_config))),
    )
    .map(|(socket, _)| socket)
    .map_err(|error| match error {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Io(error))
            if is_cancellation(&error) =>
        {
            RithmicSessionError::Cancelled
        }
        tungstenite::HandshakeError::Failure(tungstenite::Error::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            RithmicSessionError::Deadline
        }
        tungstenite::HandshakeError::Failure(tungstenite::Error::Tls(_)) => {
            RithmicSessionError::Tls
        }
        _ => RithmicSessionError::Handshake,
    })
}

pub(crate) fn set_deadline(socket: &mut RithmicWebSocket, deadline: Instant) {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_deadline(deadline),
        MaybeTlsStream::Rustls(stream) => stream.get_mut().set_deadline(deadline),
        _ => {}
    }
}

pub(crate) fn stop_requested(socket: &RithmicWebSocket) -> bool {
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream.stop_requested(),
        MaybeTlsStream::Rustls(stream) => stream.get_ref().stop_requested(),
        _ => false,
    }
}

pub(crate) fn begin_shutdown(socket: &mut RithmicWebSocket, deadline: Instant) {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.begin_shutdown(deadline),
        MaybeTlsStream::Rustls(stream) => stream.get_mut().begin_shutdown(deadline),
        _ => {}
    }
}

pub(crate) struct DeadlineTcpStream {
    stream: Arc<TcpStream>,
    deadline: Instant,
    stop: Option<Arc<AtomicBool>>,
}

impl DeadlineTcpStream {
    pub(crate) const fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
    }

    pub(crate) fn begin_shutdown(&mut self, deadline: Instant) {
        self.deadline = deadline;
        self.stop = None;
    }

    fn stop_requested(&self) -> bool {
        self.stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
    }

    fn operation_timeout(&self) -> io::Result<Duration> {
        if self.stop_requested() {
            // rustls retries `Interrupted` inside `complete_io` without
            // returning, so a stop must surface as a terminal error kind.
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                StopRequested,
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline"));
        }
        Ok(remaining.min(NETWORK_POLL_INTERVAL))
    }
}

impl Read for DeadlineTcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            self.stream
                .set_read_timeout(Some(self.operation_timeout()?))?;
            match (&*self.stream).read(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

impl Write for DeadlineTcpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            self.stream
                .set_write_timeout(Some(self.operation_timeout()?))?;
            match (&*self.stream).write(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        loop {
            self.stream
                .set_write_timeout(Some(self.operation_timeout()?))?;
            match (&*self.stream).flush() {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

fn connect_tcp(
    host: &str,
    port: u16,
    deadline: Instant,
    stop: Option<Arc<AtomicBool>>,
) -> Result<DeadlineTcpStream, RithmicSessionError> {
    let addresses = resolve_addresses(host, port, deadline, stop.as_deref())?;
    if addresses.is_empty() {
        return Err(RithmicSessionError::Resolve);
    }
    loop {
        if stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Err(RithmicSessionError::Cancelled);
        }
        for (index, address) in addresses.iter().enumerate() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RithmicSessionError::Connect);
            }
            let attempts_left =
                u32::try_from(addresses.len() - index).map_err(|_| RithmicSessionError::Connect)?;
            let attempt_timeout = connect_attempt_timeout(remaining, attempts_left);
            match connect_address(*address, attempt_timeout) {
                Ok(stream) => {
                    stream
                        .set_nodelay(true)
                        .map_err(|_| RithmicSessionError::Connect)?;
                    return Ok(DeadlineTcpStream {
                        stream: Arc::new(stream),
                        deadline,
                        stop,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Err(RithmicSessionError::Cancelled);
                }
                Err(_) => {}
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RithmicSessionError::Connect);
        }
        thread::sleep(remaining.min(NETWORK_POLL_INTERVAL));
    }
}

fn connect_attempt_timeout(remaining: Duration, attempts_left: u32) -> Duration {
    (remaining / attempts_left).min(TCP_CONNECT_ATTEMPT_LIMIT)
}

fn connect_address(address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    socket.connect_timeout(&address.into(), timeout)?;
    Ok(socket.into())
}

fn resolve_addresses(
    host: &str,
    port: u16,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<Vec<SocketAddr>, RithmicSessionError> {
    aeris_platform_runtime::resolve_addresses(host, port, deadline, stop).map_err(|error| {
        match error.kind() {
            io::ErrorKind::Interrupted => RithmicSessionError::Cancelled,
            io::ErrorKind::TimedOut => RithmicSessionError::Deadline,
            _ => RithmicSessionError::Resolve,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_connect_attempts_are_not_limited_to_the_io_poll_interval() {
        assert_eq!(
            connect_attempt_timeout(Duration::from_secs(15), 1),
            TCP_CONNECT_ATTEMPT_LIMIT
        );
        assert!(TCP_CONNECT_ATTEMPT_LIMIT > NETWORK_POLL_INTERVAL);
        assert_eq!(
            connect_attempt_timeout(Duration::from_millis(600), 2),
            Duration::from_millis(300)
        );
    }
}
