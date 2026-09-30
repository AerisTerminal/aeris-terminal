//! Cancellable HTTPS client shared by provider adapters.
//!
//! Cancellation reaches DNS, TCP, TLS and response reads. One exclusive client
//! owns the request token and its keep-alive pool; transports never outlive it.
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use ureq::unversioned::{
    resolver::{ResolvedSocketAddrs, Resolver},
    transport::{
        Buffers, ConnectProxyConnector, ConnectionDetails, Connector, Either, LazyBuffers,
        NextTimeout, RustlsConnector, Transport,
    },
};

const POLL: Duration = Duration::from_millis(50);
#[derive(Clone, Debug)]
struct Cancellation(Arc<Mutex<Arc<AtomicBool>>>);
impl Cancellation {
    fn token(&self) -> Arc<AtomicBool> {
        Arc::clone(
            &self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
    fn check(&self) -> Result<(), ureq::Error> {
        if self.token().load(Ordering::Acquire) {
            Err(io::Error::new(io::ErrorKind::ConnectionAborted, "HTTP request cancelled").into())
        } else {
            Ok(())
        }
    }
}

/// Exclusive reusable HTTP client for one provider-owned worker.
pub struct CancellableHttpClient {
    agent: ureq::Agent,
    cancellation: Cancellation,
}
impl Default for CancellableHttpClient {
    fn default() -> Self {
        let cancellation = Cancellation(Arc::new(Mutex::new(Arc::new(AtomicBool::new(false)))));
        let connector =
            ().chain(ConnectProxyConnector::default())
                .chain(CancellableConnector(cancellation.clone()))
                .chain(RustlsConnector::default());
        let config = ureq::Agent::config_builder()
            .max_idle_connections(1)
            .max_idle_connections_per_host(1)
            .build();
        Self {
            agent: ureq::Agent::with_parts(
                config,
                connector,
                CancellableResolver(cancellation.clone()),
            ),
            cancellation,
        }
    }
}
impl CancellableHttpClient {
    /// Binds every subsequent request to `stop`; setting it aborts DNS, TCP,
    /// TLS, and response reads of an in-flight request.
    pub fn set_cancellation(&mut self, stop: &Arc<AtomicBool>) {
        *self
            .cancellation
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(stop);
    }

    /// The cancellable agent that issues requests for this client.
    #[must_use]
    pub const fn agent(&self) -> &ureq::Agent {
        &self.agent
    }

    /// Releases the agent for a process-wide client that is never cancelled.
    #[must_use]
    pub fn into_agent(self) -> ureq::Agent {
        self.agent
    }
}

fn deadline(timeout: NextTimeout) -> Instant {
    Instant::now()
        + timeout
            .not_zero()
            .map_or(Duration::from_secs(15), |value| *value)
}
fn remaining(end: Instant, timeout: NextTimeout) -> Result<Duration, ureq::Error> {
    let left = end.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(ureq::Error::Timeout(timeout.reason))
    } else {
        Ok(left)
    }
}
fn would_wait(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[derive(Debug)]
struct CancellableResolver(Cancellation);
impl Resolver for CancellableResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        self.0.check()?;
        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let port = uri
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("http") {
                80
            } else {
                443
            });
        let addresses =
            crate::resolve_addresses(host, port, deadline(timeout), Some(&self.0.token()))?;
        if addresses.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        let mut result = self.empty();
        for address in addresses {
            result.push(address);
        }
        Ok(result)
    }
}

#[derive(Debug)]
struct CancellableConnector(Cancellation);
impl<In: Transport> Connector<In> for CancellableConnector {
    type Out = Either<In, CancellableTransport>;
    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        if let Some(transport) = chained {
            return Ok(Some(Either::A(transport)));
        }
        let end = deadline(details.timeout);
        for address in &details.addrs {
            self.0.check()?;
            let left = remaining(end, details.timeout)?;
            if let Ok(stream) =
                TcpStream::connect_timeout(address, left.min(Duration::from_millis(500)))
            {
                stream.set_nodelay(true)?;
                return Ok(Some(Either::B(CancellableTransport {
                    stream,
                    cancellation: self.0.clone(),
                    buffers: LazyBuffers::new(
                        details.config.input_buffer_size(),
                        details.config.output_buffer_size(),
                    ),
                })));
            }
        }
        self.0.check()?;
        Err(ureq::Error::ConnectionFailed)
    }
}

#[derive(Debug)]
struct CancellableTransport {
    stream: TcpStream,
    cancellation: Cancellation,
    buffers: LazyBuffers,
}
impl Transport for CancellableTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let end = deadline(timeout);
        let mut sent = 0;
        while sent < amount {
            self.cancellation.check()?;
            self.stream
                .set_write_timeout(Some(POLL.min(remaining(end, timeout)?)))?;
            match self.stream.write(&self.buffers.output()[sent..amount]) {
                Ok(0) => {
                    return Err(
                        io::Error::new(io::ErrorKind::WriteZero, "HTTP write stopped").into(),
                    );
                }
                Ok(count) => sent += count,
                Err(error) if would_wait(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let end = deadline(timeout);
        loop {
            self.cancellation.check()?;
            self.stream
                .set_read_timeout(Some(POLL.min(remaining(end, timeout)?)))?;
            match self.stream.read(self.buffers.input_append_buf()) {
                Ok(count) => {
                    self.buffers.input_appended(count);
                    return Ok(count > 0);
                }
                Err(error) if would_wait(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    fn is_open(&mut self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let open = self
            .stream
            .peek(&mut [0])
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock);
        self.stream.set_nonblocking(false).is_ok() && open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc, thread};

    fn cancellation_interrupts_stalled_http(body: bool) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted, ready) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            if body {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
                    .unwrap();
            }
            accepted.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(3));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (result, received) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut client = CancellableHttpClient::default();
            client.set_cancellation(&worker_stop);
            let response = client
                .agent()
                .post(format!("http://{address}"))
                .config()
                .timeout_global(Some(Duration::from_secs(15)))
                .build()
                .send("test");
            let outcome = response.map_err(|_| ()).and_then(|mut response| {
                response
                    .body_mut()
                    .as_reader()
                    .read_to_end(&mut Vec::new())
                    .map(|_| ())
                    .map_err(|_| ())
            });
            result.send(outcome).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        stop.store(true, Ordering::Release);
        assert!(
            received
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        release.send(()).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn cancellation_interrupts_response_headers() {
        cancellation_interrupts_stalled_http(false);
    }
    #[test]
    fn cancellation_interrupts_response_body() {
        cancellation_interrupts_stalled_http(true);
    }
}
