//! Bounded synchronous WebSocket session against the live endpoint.
//!
//! The session subscribes to `heartbeats` and `market_trades`, reads with an
//! absolute deadline, and reports explicit outcomes. Reconnect and resnapshot
//! decisions belong to the caller; a sequence gap is surfaced, never hidden.

use crate::config::CoinbaseConfig;
use crate::decoder::{CanonicalTrade, CoinbaseDecoder, DecoderMetrics};
use crate::errors::CoinbaseError;
use crate::history::{coinbase_tls_config, connect_coinbase_endpoint_cancellable};
use crate::messages::subscribe_frame;
use crate::review::{WEBSOCKET_ENDPOINT, WEBSOCKET_HOST};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tungstenite::{Connector, Message, WebSocket, stream::MaybeTlsStream};

const WEBSOCKET_PORT: u16 = 443;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(5);

/// How one bounded session ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionOutcome {
    /// The requested collection window completed.
    Completed,
    /// A sequence gap forced an explicit stop; reconnect and resnapshot.
    SequenceGap,
    /// The server closed or errored the connection.
    ClosedByPeer,
    /// No subscribed channel message arrived within the bounded liveness window.
    InactivityTimeout,
}

/// Health counters for one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHealth {
    pub outcome: SessionOutcome,
    pub metrics: DecoderMetrics,
}

/// One bounded live session.
pub struct CoinbaseSession {
    config: CoinbaseConfig,
}

impl CoinbaseSession {
    /// Creates a session for one validated configuration.
    #[must_use]
    pub const fn new(config: CoinbaseConfig) -> Self {
        Self { config }
    }

    /// Opens and subscribes one direct provider connection.
    ///
    /// # Errors
    ///
    /// Returns an error for transport, TLS, handshake, or subscription failure.
    pub fn connect(&self) -> Result<CoinbaseConnection, CoinbaseError> {
        self.connect_with_stop(None)
    }

    /// Opens and subscribes one direct provider connection that observes a
    /// cooperative stop request during DNS, TCP, TLS, and subscription I/O.
    ///
    /// # Errors
    ///
    /// Returns an error for cancellation, transport, TLS, handshake, or
    /// subscription failure.
    pub fn connect_cancellable(
        &self,
        stop: Arc<AtomicBool>,
    ) -> Result<CoinbaseConnection, CoinbaseError> {
        self.connect_with_stop(Some(stop))
    }

    fn connect_with_stop(
        &self,
        stop: Option<Arc<AtomicBool>>,
    ) -> Result<CoinbaseConnection, CoinbaseError> {
        let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
        let tcp = connect_coinbase_endpoint_cancellable(
            WEBSOCKET_HOST,
            WEBSOCKET_PORT,
            connect_deadline,
            stop,
        )
        .map_err(CoinbaseError::Transport)?;
        let connector = Connector::Rustls(Arc::new(
            coinbase_tls_config().map_err(CoinbaseError::Transport)?,
        ));
        let (mut socket, _response) =
            tungstenite::client_tls_with_config(WEBSOCKET_ENDPOINT, tcp, None, Some(connector))
                .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
        let channels = if self.config.include_level2 {
            &["heartbeats", "market_trades", "level2"][..]
        } else {
            &["heartbeats", "market_trades"][..]
        };
        for channel in channels {
            socket
                .send(Message::Text(
                    subscribe_frame(&self.config.products, channel).into(),
                ))
                .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
        }
        Ok(CoinbaseConnection {
            socket,
            maximum_message_bytes: self.config.maximum_message_bytes,
            decoder: CoinbaseDecoder::new(),
        })
    }

    /// Collects trades for `window`, invoking `on_trade` for each canonical
    /// trade. Returns aggregate health when the window closes.
    ///
    /// # Errors
    ///
    /// Returns an error for transport, subscription, or deadline failures.
    pub fn collect(
        &self,
        window: Duration,
        on_trade: &mut impl FnMut(&CanonicalTrade),
    ) -> Result<SessionHealth, CoinbaseError> {
        self.connect()?
            .collect_until(window, &mut || false, on_trade)
    }
}

/// One subscribed Coinbase WebSocket connection with bounded cancellation latency.
pub struct CoinbaseConnection {
    socket: WebSocket<MaybeTlsStream<crate::history::DeadlineTcpStream>>,
    maximum_message_bytes: usize,
    decoder: CoinbaseDecoder,
}

impl CoinbaseConnection {
    /// Collects trades until the window ends, the provider invalidates the
    /// stream, or `should_stop` requests cooperative shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error for transport, protocol, or deadline failure.
    pub fn collect_until(
        self,
        window: Duration,
        should_stop: &mut impl FnMut() -> bool,
        on_trade: &mut impl FnMut(&CanonicalTrade),
    ) -> Result<SessionHealth, CoinbaseError> {
        let deadline = Instant::now() + window;
        self.collect_with_deadline(Some(deadline), should_stop, on_trade, &mut || {})
    }

    /// Collects trades until the provider invalidates the stream or
    /// `should_stop` requests cooperative shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error for transport or protocol failure.
    pub fn collect_until_stopped(
        self,
        should_stop: &mut impl FnMut() -> bool,
        on_trade: &mut impl FnMut(&CanonicalTrade),
    ) -> Result<SessionHealth, CoinbaseError> {
        self.collect_with_deadline(None, should_stop, on_trade, &mut || {})
    }

    /// Collects trades, heartbeat liveness, and raw bounded Level 2 messages.
    ///
    /// The Level 2 callback executes on the provider session thread; callers
    /// must decode or enqueue it without blocking.
    /// Collects public trades, heartbeats, and Level 2 payloads until cancellation.
    ///
    /// # Errors
    ///
    /// Returns a protocol, transport, decoding, sequence, or inactivity failure.
    pub fn collect_until_stopped_with_market_events(
        self,
        should_stop: &mut impl FnMut() -> bool,
        on_trade: &mut impl FnMut(&CanonicalTrade),
        on_heartbeat: &mut impl FnMut(),
        on_level2: &mut impl FnMut(&[u8]),
    ) -> Result<SessionHealth, CoinbaseError> {
        self.collect_with_deadline_and_level2(None, should_stop, on_trade, on_heartbeat, on_level2)
    }

    fn collect_with_deadline(
        self,
        deadline: Option<Instant>,
        should_stop: &mut impl FnMut() -> bool,
        on_trade: &mut impl FnMut(&CanonicalTrade),
        on_heartbeat: &mut impl FnMut(),
    ) -> Result<SessionHealth, CoinbaseError> {
        self.collect_with_deadline_and_level2(
            deadline,
            should_stop,
            on_trade,
            on_heartbeat,
            &mut |_| {},
        )
    }

    fn collect_with_deadline_and_level2(
        mut self,
        deadline: Option<Instant>,
        should_stop: &mut impl FnMut() -> bool,
        on_trade: &mut impl FnMut(&CanonicalTrade),
        on_heartbeat: &mut impl FnMut(),
        on_level2: &mut impl FnMut(&[u8]),
    ) -> Result<SessionHealth, CoinbaseError> {
        self.collect_with_limits(
            deadline,
            INACTIVITY_TIMEOUT,
            should_stop,
            on_trade,
            on_heartbeat,
            on_level2,
        )
    }

    fn collect_with_limits(
        &mut self,
        deadline: Option<Instant>,
        inactivity_timeout: Duration,
        should_stop: &mut impl FnMut() -> bool,
        on_trade: &mut impl FnMut(&CanonicalTrade),
        on_heartbeat: &mut impl FnMut(),
        on_level2: &mut impl FnMut(&[u8]),
    ) -> Result<SessionHealth, CoinbaseError> {
        let mut outcome = SessionOutcome::Completed;
        let mut last_message = Instant::now();
        loop {
            let now = Instant::now();
            if should_stop() || deadline.is_some_and(|limit| now >= limit) {
                break;
            }
            let inactivity_remaining =
                inactivity_timeout.saturating_sub(now.duration_since(last_message));
            if inactivity_remaining.is_zero() {
                outcome = SessionOutcome::InactivityTimeout;
                break;
            }
            let poll_window = deadline
                .map_or(STOP_POLL_INTERVAL, |limit| {
                    limit.saturating_duration_since(now)
                })
                .min(STOP_POLL_INTERVAL)
                .min(inactivity_remaining);
            self.set_socket_deadline(now + poll_window);
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if text.len() > self.maximum_message_bytes {
                        return Err(CoinbaseError::InvalidMessage);
                    }
                    match self.decoder.decode_with_liveness(text.as_bytes()) {
                        Ok((trades, subscribed_channel, heartbeat, level2)) => {
                            if subscribed_channel {
                                last_message = Instant::now();
                            }
                            if heartbeat {
                                on_heartbeat();
                            }
                            if level2 {
                                on_level2(text.as_bytes());
                            }
                            for trade in &trades {
                                on_trade(trade);
                            }
                        }
                        Err(CoinbaseError::SequenceGap { .. }) => {
                            outcome = SessionOutcome::SequenceGap;
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(Message::Ping(payload)) => {
                    let now = Instant::now();
                    if deadline.is_some_and(|limit| now >= limit) {
                        break;
                    }
                    let inactivity_remaining =
                        inactivity_timeout.saturating_sub(now.duration_since(last_message));
                    if inactivity_remaining.is_zero() {
                        outcome = SessionOutcome::InactivityTimeout;
                        break;
                    }
                    let write_window = deadline
                        .map_or(STOP_POLL_INTERVAL, |limit| {
                            limit.saturating_duration_since(now)
                        })
                        .min(STOP_POLL_INTERVAL)
                        .min(inactivity_remaining);
                    self.set_socket_deadline(now + write_window);
                    self.socket
                        .send(Message::Pong(payload))
                        .map_err(|error| CoinbaseError::Transport(error.to_string()))?;
                }
                Ok(Message::Close(_)) => {
                    outcome = SessionOutcome::ClosedByPeer;
                    break;
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if deadline.is_some_and(|limit| Instant::now() >= limit) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(CoinbaseError::Transport(error.to_string())),
            }
        }
        let _ = self.socket.close(None);
        Ok(SessionHealth {
            outcome,
            metrics: self.decoder.metrics(),
        })
    }

    fn set_socket_deadline(&mut self, deadline: Instant) {
        match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream.set_deadline(deadline),
            MaybeTlsStream::Rustls(stream) => stream.get_mut().set_deadline(deadline),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CoinbaseConnection, CoinbaseDecoder, SessionOutcome};
    use crate::history::connect_coinbase_endpoint_cancellable;
    use std::{
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    use tungstenite::{WebSocket, protocol::Role, stream::MaybeTlsStream};

    #[test]
    fn silent_connection_expires_at_the_inactivity_bound() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind silent peer");
        let address = listener.local_addr().expect("read silent peer address");
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("accept silent peer");
            thread::sleep(Duration::from_millis(300));
        });
        let connect_deadline = Instant::now() + Duration::from_secs(1);
        let stream = connect_coinbase_endpoint_cancellable(
            "127.0.0.1",
            address.port(),
            connect_deadline,
            None,
        )
        .expect("connect silent peer");
        let mut connection = CoinbaseConnection {
            socket: WebSocket::from_raw_socket(MaybeTlsStream::Plain(stream), Role::Client, None),
            maximum_message_bytes: 1024,
            decoder: CoinbaseDecoder::new(),
        };
        let started = Instant::now();
        let health = connection
            .collect_with_limits(
                None,
                Duration::from_millis(100),
                &mut || false,
                &mut |_| {},
                &mut || {},
                &mut |_| {},
            )
            .expect("silence returns bounded health");
        assert_eq!(health.outcome, SessionOutcome::InactivityTimeout);
        assert!(started.elapsed() < Duration::from_millis(500));
        server.join().expect("join silent peer");
    }

    #[test]
    fn control_frames_do_not_mask_subscribed_channel_inactivity() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind control-frame peer");
        let address = listener.local_addr().expect("read peer address");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept control-frame peer");
            let mut socket = WebSocket::from_raw_socket(stream, Role::Server, None);
            for sequence in 0..8 {
                if socket
                    .send(tungstenite::Message::Ping(vec![sequence].into()))
                    .is_err()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let connect_deadline = Instant::now() + Duration::from_secs(1);
        let stream = connect_coinbase_endpoint_cancellable(
            "127.0.0.1",
            address.port(),
            connect_deadline,
            None,
        )
        .expect("connect control-frame peer");
        let mut connection = CoinbaseConnection {
            socket: WebSocket::from_raw_socket(MaybeTlsStream::Plain(stream), Role::Client, None),
            maximum_message_bytes: 1024,
            decoder: CoinbaseDecoder::new(),
        };
        let started = Instant::now();
        let health = connection
            .collect_with_limits(
                None,
                Duration::from_millis(100),
                &mut || false,
                &mut |_| {},
                &mut || {},
                &mut |_| {},
            )
            .expect("control traffic returns bounded health");
        assert_eq!(health.outcome, SessionOutcome::InactivityTimeout);
        assert!(started.elapsed() < Duration::from_millis(300));
        server.join().expect("join control-frame peer");
    }
}
