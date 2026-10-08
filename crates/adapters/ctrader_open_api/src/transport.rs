//! Bounded blocking TLS transport. The reader and writer share one TLS state;
//! short socket polls release the lock so neither direction starves the other.
use crate::{ProtoMessage, codec, host::CtraderHost};
use prost::Message;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const OUTBOUND_CAPACITY: usize = 256;
const INBOUND_CAPACITY: usize = 16;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const POLL: Duration = Duration::from_millis(100);
const READ_DEADLINE: Duration = Duration::from_secs(30);
const HEARTBEAT_AFTER: Duration = Duration::from_secs(8);
// Allow for socket scheduling delay between the writer and the provider's
// receive timestamp: a burst observed by the server must not exceed its cap.
const RATE_WINDOW: Duration = Duration::from_millis(1100);
type TlsStream = StreamOwned<ClientConnection, TcpStream>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Cancelled,
    Connect,
    Tls,
    Io,
    ReadTimeout,
    Closed,
    Overflow,
    RequestTooLarge,
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cTrader transport: {self:?}")
    }
}
impl std::error::Error for TransportError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bucket {
    General,
    Historical,
}

struct Outbound {
    frame: ProtoMessage,
    bucket: Bucket,
}

/// Sliding one-second limiter (stronger than a fixed one-second clock bucket).
struct RateBucket {
    sent: VecDeque<Instant>,
    capacity: usize,
    min_gap: Duration,
}
impl RateBucket {
    fn new(capacity: usize) -> Self {
        Self {
            sent: VecDeque::with_capacity(capacity),
            capacity,
            min_gap: if capacity == 4 {
                Duration::from_millis(280)
            } else {
                Duration::from_millis(26)
            },
        }
    }
    fn wait(&mut self, stop: &AtomicBool, closed: &AtomicBool) -> Result<(), TransportError> {
        loop {
            let now = Instant::now();
            while self
                .sent
                .front()
                .is_some_and(|at| now.duration_since(*at) >= RATE_WINDOW)
            {
                self.sent.pop_front();
            }
            let gap = self.sent.back().map_or(Duration::ZERO, |at| {
                self.min_gap.saturating_sub(now.duration_since(*at))
            });
            if self.sent.len() < self.capacity && gap.is_zero() {
                self.sent.push_back(now);
                return Ok(());
            }
            if stop.load(Ordering::Acquire) || closed.load(Ordering::Acquire) {
                return Err(TransportError::Cancelled);
            }
            let capacity_wait = if self.sent.len() == self.capacity {
                RATE_WINDOW.saturating_sub(now.duration_since(self.sent[0]))
            } else {
                Duration::ZERO
            };
            thread::sleep(POLL.min(gap.max(capacity_wait)));
        }
    }
}

pub struct Transport {
    outbound: SyncSender<Outbound>,
    inbound: Receiver<Result<ProtoMessage, TransportError>>,
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    socket: TcpStream,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    pauses: Arc<Mutex<[Option<Instant>; 2]>>,
}
impl Transport {
    /// Connect to a production endpoint with system roots and the protocol port.
    ///
    /// # Errors
    /// Returns a connection, TLS, or cancellation fault.
    pub fn connect(host: CtraderHost, stop: Arc<AtomicBool>) -> Result<Self, TransportError> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let address = aeris_platform_runtime::resolve_addresses(
            host.hostname(),
            CtraderHost::PORT,
            Instant::now() + Duration::from_secs(1),
            Some(&stop),
        )
        .map_err(|_| {
            if stop.load(Ordering::Acquire) {
                TransportError::Cancelled
            } else {
                TransportError::Connect
            }
        })?
        .into_iter()
        .next()
        .ok_or(TransportError::Connect)?;
        Self::connect_to(address, host.hostname(), roots, stop)
    }

    /// Pin roots and address for a local scripted TLS server.
    ///
    /// # Errors
    /// Returns a connection, TLS, or cancellation fault.
    pub fn connect_to(
        address: SocketAddr,
        hostname: &str,
        roots: RootCertStore,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, TransportError> {
        if stop.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        let socket = Socket::new(
            Domain::for_address(address),
            Type::STREAM,
            Some(Protocol::TCP),
        )
        .map_err(|_| TransportError::Connect)?;
        socket
            .set_tcp_nodelay(true)
            .map_err(|_| TransportError::Connect)?;
        socket
            .set_keepalive(true)
            .map_err(|_| TransportError::Connect)?;
        socket
            .set_tcp_keepalive(&TcpKeepalive::new().with_time(Duration::from_secs(30)))
            .map_err(|_| TransportError::Connect)?;
        socket
            .connect_timeout(&address.into(), Duration::from_millis(500))
            .map_err(|_| {
                if stop.load(Ordering::Acquire) {
                    TransportError::Cancelled
                } else {
                    TransportError::Connect
                }
            })?;
        let tcp: TcpStream = socket.into();
        tcp.set_read_timeout(Some(POLL))
            .map_err(|_| TransportError::Connect)?;
        tcp.set_write_timeout(Some(Duration::from_millis(500)))
            .map_err(|_| TransportError::Connect)?;
        let shutdown = tcp.try_clone().map_err(|_| TransportError::Connect)?;
        let config = tls_configuration(roots)?;
        let name = ServerName::try_from(hostname.to_owned()).map_err(|_| TransportError::Tls)?;
        let connection =
            ClientConnection::new(Arc::new(config), name).map_err(|_| TransportError::Tls)?;
        let mut stream = StreamOwned::new(connection, tcp);
        let deadline = Instant::now() + Duration::from_millis(1500);
        while stream.conn.is_handshaking() {
            if stop.load(Ordering::Acquire) {
                return Err(TransportError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(TransportError::Tls);
            }
            match stream.conn.complete_io(&mut stream.sock) {
                Ok(_) => {}
                Err(error) if retryable(&error) => {}
                Err(_) => return Err(TransportError::Tls),
            }
        }
        let stream = Arc::new(Mutex::new(stream));
        let closed = Arc::new(AtomicBool::new(false));
        let pauses = Arc::new(Mutex::new([None, None]));
        let (outbound, requests) = mpsc::sync_channel::<Outbound>(OUTBOUND_CAPACITY);
        let (responses, inbound) = mpsc::sync_channel(INBOUND_CAPACITY);
        let reader_stream = Arc::clone(&stream);
        let reader_stop = Arc::clone(&stop);
        let reader_closed = Arc::clone(&closed);
        let reader = thread::Builder::new()
            .name("ctrader-tls-reader".into())
            .spawn(move || read_worker(&reader_stream, &reader_stop, &reader_closed, &responses))
            .map_err(|_| TransportError::Connect)?;
        let writer_stop = Arc::clone(&stop);
        let writer_closed = Arc::clone(&closed);
        let writer_stream = Arc::clone(&stream);
        let writer_pauses = Arc::clone(&pauses);
        let Ok(writer) = thread::Builder::new()
            .name("ctrader-tls-writer".into())
            .spawn(move || {
                write_worker(
                    &writer_stream,
                    &writer_stop,
                    &writer_closed,
                    &writer_pauses,
                    &requests,
                );
            })
        else {
            closed.store(true, Ordering::Release);
            let _ = shutdown.shutdown(Shutdown::Both);
            let _ = reader.join();
            return Err(TransportError::Connect);
        };
        Ok(Self {
            outbound,
            inbound,
            stop,
            closed,
            socket: shutdown,
            reader: Some(reader),
            writer: Some(writer),
            pauses,
        })
    }

    /// # Errors
    /// Returns an explicit full-queue, closed-transport, or cancellation error.
    pub fn send(&self, frame: ProtoMessage, bucket: Bucket) -> Result<(), TransportError> {
        if self.stop.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire) {
            return Err(TransportError::Cancelled);
        }
        if frame.encoded_len() > MAX_REQUEST_BYTES {
            return Err(TransportError::RequestTooLarge);
        }
        self.outbound
            .try_send(Outbound { frame, bucket })
            .map_err(|error| match error {
                TrySendError::Full(_) => TransportError::Overflow,
                TrySendError::Disconnected(_) => TransportError::Closed,
            })
    }
    /// # Errors
    /// Returns a timeout or transport error.
    pub fn receive(&self, timeout: Duration) -> Result<ProtoMessage, TransportError> {
        self.inbound
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => TransportError::ReadTimeout,
                // The reader exits when cancelled or closed; that is not a dropped socket.
                mpsc::RecvTimeoutError::Disconnected
                    if self.stop.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire) =>
                {
                    TransportError::Cancelled
                }
                mpsc::RecvTimeoutError::Disconnected => TransportError::Closed,
            })?
    }
    pub fn close(&mut self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.socket.shutdown(Shutdown::Both);
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
    /// Pause one request bucket after a provider rate-limit response.
    pub fn pause(&self, bucket: Bucket, duration: Duration) {
        if let Ok(mut pauses) = self.pauses.lock() {
            let index = usize::from(bucket == Bucket::Historical);
            let deadline = Instant::now() + duration;
            pauses[index] = Some(pauses[index].map_or(deadline, |old| old.max(deadline)));
        }
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.close();
    }
}
fn read_worker(
    stream: &Arc<Mutex<TlsStream>>,
    stop: &Arc<AtomicBool>,
    closed: &Arc<AtomicBool>,
    responses: &SyncSender<Result<ProtoMessage, TransportError>>,
) {
    let mut last_inbound = Instant::now();
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        if stop.load(Ordering::Acquire) || closed.load(Ordering::Acquire) {
            break;
        }
        let result = match stream.lock() {
            Ok(mut guard) => guard.read(&mut chunk),
            Err(_) => break,
        };
        match result {
            Ok(0) => {
                let _ = responses.try_send(Err(TransportError::Closed));
                break;
            }
            Ok(count) => {
                last_inbound = Instant::now();
                bytes.extend_from_slice(&chunk[..count]);
                loop {
                    if bytes.len() < 4 {
                        break;
                    }
                    let length =
                        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
                    if length > codec::MAXIMUM_FRAME_BYTES {
                        let _ = responses.try_send(Err(TransportError::Io));
                        return;
                    }
                    if bytes.len() < 4 + length {
                        break;
                    }
                    let frame = codec::read_frame(&mut io::Cursor::new(&bytes[..4 + length]));
                    bytes.drain(..4 + length);
                    if let Ok(frame) = frame {
                        if frame.payload_type == 51 {
                            continue;
                        }
                        if responses.try_send(Ok(frame)).is_ok() {
                            continue;
                        }
                    }
                    let _ = stream
                        .lock()
                        .map(|guard| guard.sock.shutdown(Shutdown::Both));
                    return;
                }
            }
            Err(error) if retryable(&error) => {
                if last_inbound.elapsed() >= READ_DEADLINE {
                    let _ = responses.try_send(Err(TransportError::ReadTimeout));
                    break;
                }
                // A series of timed-out reads must not monopolize rustls.
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                let _ = responses.try_send(Err(TransportError::Io));
                break;
            }
        }
    }
}
fn write_worker(
    stream: &Arc<Mutex<TlsStream>>,
    stop: &Arc<AtomicBool>,
    closed: &Arc<AtomicBool>,
    pauses: &Arc<Mutex<[Option<Instant>; 2]>>,
    requests: &Receiver<Outbound>,
) {
    let mut general = RateBucket::new(45);
    let mut historical = RateBucket::new(4);
    let mut last_write = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) || closed.load(Ordering::Acquire) {
            break;
        }
        let outbound = match requests.recv_timeout(POLL) {
            Ok(request) => Some(request),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let frame = if let Some(outbound) = outbound {
            let index = usize::from(outbound.bucket == Bucket::Historical);
            loop {
                if stop.load(Ordering::Acquire) || closed.load(Ordering::Acquire) {
                    return;
                }
                let until = pauses.lock().ok().and_then(|pauses| pauses[index]);
                if until.is_none_or(|until| until <= Instant::now()) {
                    break;
                }
                if last_write.elapsed() >= HEARTBEAT_AFTER {
                    let heartbeat = ProtoMessage {
                        payload_type: 51,
                        payload: None,
                        client_msg_id: None,
                    };
                    if !stream
                        .lock()
                        .is_ok_and(|mut guard| codec::write_frame(&mut *guard, &heartbeat).is_ok())
                    {
                        return;
                    }
                    last_write = Instant::now();
                }
                thread::sleep(POLL);
            }
            if outbound.bucket == Bucket::Historical && historical.wait(stop, closed).is_err() {
                break;
            }
            if general.wait(stop, closed).is_err() {
                break;
            }
            outbound.frame
        } else if last_write.elapsed() >= HEARTBEAT_AFTER {
            ProtoMessage {
                payload_type: 51,
                payload: None,
                client_msg_id: None,
            }
        } else {
            continue;
        };
        let wrote = stream
            .lock()
            .is_ok_and(|mut guard| codec::write_frame(&mut *guard, &frame).is_ok());
        if !wrote {
            break;
        }
        last_write = Instant::now();
    }
}
fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
fn tls_configuration(roots: RootCertStore) -> Result<ClientConfig, TransportError> {
    Ok(
        ClientConfig::builder_with_provider(
            Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        )
        .with_safe_default_protocol_versions()
        .map_err(|_| TransportError::Tls)?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}
