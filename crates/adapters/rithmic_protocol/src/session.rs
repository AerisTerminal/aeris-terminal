use crate::{
    DecodedCatalogMessage, DecodedControlMessage, DecodedHistoryMessage, DecodedMarketMessage,
    InstrumentReferenceRequest, LoginRequest, MarketDataSubscription, OutboundRequest,
    ProtocolError, ReadOnlyPlant, ReplayKind, RithmicProtocolBackend, RithmicProtocolCodec,
    RithmicSessionError, RithmicSessionLimits, SymbolSearchRequest, TickBarReplayRequest,
    TimeBarReplayRequest,
    endpoint::RithmicEndpoint,
    network::{
        ConnectionAbort, RithmicWebSocket, begin_shutdown, connect_websocket, default_tls_config,
        set_deadline,
    },
};
use rustls::ClientConfig;
use std::{
    fmt,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tungstenite::{Bytes, Message};

const TEST_SYSTEM: &str = "Rithmic Test";

struct ConnectionControl<'a> {
    stop: Option<Arc<AtomicBool>>,
    abort: Option<&'a ConnectionAbort>,
}

/// Borrowed credentials used only during one synchronous connection attempt.
#[derive(Clone, Copy)]
pub struct RithmicCredentials<'a> {
    pub user: &'a str,
    pub password: &'a str,
}

impl fmt::Debug for RithmicCredentials<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicCredentials")
            .field("user", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Non-secret application identity sent during login.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicApplication<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// One sanitized inbound message from the authenticated read-only session.
#[derive(Clone, Debug, PartialEq)]
pub enum RithmicSessionMessage {
    Control(DecodedControlMessage),
    Catalog(DecodedCatalogMessage),
    Market(DecodedMarketMessage),
    History(DecodedHistoryMessage),
}

/// Fixed-endpoint Rithmic Test session connector.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RithmicTestSession;

impl RithmicTestSession {
    /// Performs system discovery on one WSS connection, closes it, then logs in
    /// to the ticker plant on a fresh WSS connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted terminal or transient session failure.
    pub fn discover_and_login(
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Option<Arc<AtomicBool>>,
    ) -> Result<RithmicTickerConnection, RithmicSessionError> {
        let tls_config = default_tls_config()?;
        Self::connect_with(
            RithmicEndpoint::TEST,
            credentials,
            application,
            limits,
            stop,
            tls_config,
        )
    }

    /// Performs system discovery, then logs in to the history plant on a fresh
    /// WSS connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted terminal or transient session failure.
    pub fn discover_and_login_history(
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Option<Arc<AtomicBool>>,
    ) -> Result<RithmicHistoryConnection, RithmicSessionError> {
        let tls_config = default_tls_config()?;
        Self::connect_plant_with(
            RithmicEndpoint::TEST,
            credentials,
            application,
            limits,
            ConnectionControl { stop, abort: None },
            tls_config,
            ReadOnlyPlant::History,
        )
        .map(RithmicHistoryConnection::new)
    }

    pub(crate) fn connect_with(
        endpoint: RithmicEndpoint,
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Option<Arc<AtomicBool>>,
        tls_config: ClientConfig,
    ) -> Result<RithmicTickerConnection, RithmicSessionError> {
        Self::connect_plant_with(
            endpoint,
            credentials,
            application,
            limits,
            ConnectionControl { stop, abort: None },
            tls_config,
            ReadOnlyPlant::Ticker,
        )
        .map(RithmicTickerConnection::new)
    }

    pub(crate) fn discover_and_login_with_abort(
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Arc<AtomicBool>,
        abort: &ConnectionAbort,
    ) -> Result<RithmicTickerConnection, RithmicSessionError> {
        let tls_config = default_tls_config()?;
        Self::connect_plant_with(
            RithmicEndpoint::TEST,
            credentials,
            application,
            limits,
            ConnectionControl {
                stop: Some(stop),
                abort: Some(abort),
            },
            tls_config,
            ReadOnlyPlant::Ticker,
        )
        .map(RithmicTickerConnection::new)
    }

    #[cfg(all(test, rithmic_kit))]
    pub(crate) fn connect_history_with(
        endpoint: RithmicEndpoint,
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Option<Arc<AtomicBool>>,
        tls_config: ClientConfig,
    ) -> Result<RithmicHistoryConnection, RithmicSessionError> {
        Self::connect_plant_with(
            endpoint,
            credentials,
            application,
            limits,
            ConnectionControl { stop, abort: None },
            tls_config,
            ReadOnlyPlant::History,
        )
        .map(RithmicHistoryConnection::new)
    }

    fn connect_plant_with(
        endpoint: RithmicEndpoint,
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        control: ConnectionControl<'_>,
        tls_config: ClientConfig,
        plant: ReadOnlyPlant,
    ) -> Result<AuthenticatedConnection, RithmicSessionError> {
        let endpoint = endpoint.validate()?;
        let limits = limits.validate()?;
        let backend = RithmicProtocolBackend::detected();
        if !backend.is_available() {
            return Err(RithmicSessionError::KitUnavailable);
        }

        let ConnectionControl { stop, abort } = control;

        let mut discovery =
            connect_websocket(endpoint, limits, stop.clone(), abort, tls_config.clone())?;
        set_deadline(&mut discovery, Instant::now() + limits.response_timeout);
        send_request(&mut discovery, &backend, OutboundRequest::DiscoverSystems)?;
        set_deadline(&mut discovery, Instant::now() + limits.response_timeout);
        let systems = read_control(&mut discovery, &backend)?;
        match systems {
            DecodedControlMessage::Systems {
                accepted: false, ..
            } => {
                return Err(RithmicSessionError::DiscoveryRejected);
            }
            DecodedControlMessage::Systems { names, .. }
                if names.iter().any(|name| name == TEST_SYSTEM) => {}
            DecodedControlMessage::Systems { .. } => {
                return Err(RithmicSessionError::TestSystemUnavailable);
            }
            _ => return Err(RithmicSessionError::UnexpectedMessage),
        }
        finish_discovery_close(&mut discovery, limits.close_timeout)?;
        drop(discovery);

        let mut socket = connect_websocket(endpoint, limits, stop, abort, tls_config)?;
        set_deadline(&mut socket, Instant::now() + limits.response_timeout);
        send_request(
            &mut socket,
            &backend,
            OutboundRequest::Login(LoginRequest {
                user: credentials.user,
                password: credentials.password,
                app_name: application.name,
                app_version: application.version,
                system_name: TEST_SYSTEM,
                plant,
            }),
        )?;
        set_deadline(&mut socket, Instant::now() + limits.response_timeout);
        let login = read_control(&mut socket, &backend)?;
        match login {
            DecodedControlMessage::Login {
                accepted: true,
                heartbeat_seconds: Some(heartbeat_seconds),
            } => {
                let heartbeat_interval = Duration::from_secs_f64(heartbeat_seconds);
                if heartbeat_interval.is_zero() {
                    return Err(RithmicSessionError::Protocol);
                }
                Ok(AuthenticatedConnection {
                    socket,
                    heartbeat_interval,
                    limits,
                })
            }
            DecodedControlMessage::Login {
                accepted: false, ..
            } => Err(RithmicSessionError::LoginRejected),
            _ => Err(RithmicSessionError::UnexpectedMessage),
        }
    }
}

struct AuthenticatedConnection {
    socket: RithmicWebSocket,
    heartbeat_interval: Duration,
    limits: RithmicSessionLimits,
}

impl AuthenticatedConnection {
    const fn heartbeat_interval(&self) -> Duration {
        self.heartbeat_interval
    }

    fn send(&mut self, request: OutboundRequest<'_>) -> Result<(), RithmicSessionError> {
        set_deadline(
            &mut self.socket,
            Instant::now() + self.limits.response_timeout,
        );
        send_request(
            &mut self.socket,
            &RithmicProtocolBackend::detected(),
            request,
        )
    }

    fn read_next(&mut self) -> Result<RithmicSessionMessage, RithmicSessionError> {
        self.read_next_until(Instant::now() + self.limits.response_timeout)
    }

    fn read_next_until(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicSessionMessage, RithmicSessionError> {
        set_deadline(&mut self.socket, deadline);
        let frame = read_binary_until(&mut self.socket, deadline)?;
        decode_session_message(&frame)
    }

    fn close(mut self) -> Result<(), RithmicSessionError> {
        let deadline = Instant::now() + self.limits.close_timeout;
        begin_shutdown(&mut self.socket, deadline);
        let backend = RithmicProtocolBackend::detected();
        send_request(&mut self.socket, &backend, OutboundRequest::Logout)?;
        let mut acknowledged = false;
        for _ in 0..64 {
            let frame = read_binary_until(&mut self.socket, deadline)?;
            let control = match backend.decode_control(&frame) {
                Ok(control) => control,
                Err(ProtocolError::UnsupportedTemplate(_)) => continue,
                Err(error) => return Err(map_protocol_error(error)),
            };
            match control {
                DecodedControlMessage::Logout { accepted: true }
                | DecodedControlMessage::ForcedLogout => {
                    acknowledged = true;
                    break;
                }
                DecodedControlMessage::Reject
                | DecodedControlMessage::Logout { accepted: false } => {
                    return Err(RithmicSessionError::Protocol);
                }
                _ => {}
            }
        }
        if !acknowledged {
            return Err(RithmicSessionError::Deadline);
        }
        self.socket
            .close(None)
            .map_err(|_| RithmicSessionError::Transport)
    }
}

/// Authenticated read-only ticker-plant connection.
pub struct RithmicTickerConnection {
    connection: AuthenticatedConnection,
    search_in_flight: bool,
}

impl fmt::Debug for RithmicTickerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicTickerConnection")
            .field("heartbeat_interval", &self.connection.heartbeat_interval)
            .field("limits", &self.connection.limits)
            .finish_non_exhaustive()
    }
}

impl RithmicTickerConnection {
    const fn new(connection: AuthenticatedConnection) -> Self {
        Self {
            connection,
            search_in_flight: false,
        }
    }

    #[must_use]
    pub const fn heartbeat_interval(&self) -> Duration {
        self.connection.heartbeat_interval()
    }

    /// Sends a provider heartbeat request.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn send_heartbeat(&mut self) -> Result<(), RithmicSessionError> {
        self.send(OutboundRequest::Heartbeat)
    }

    /// Starts one bounded symbol search.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn search_symbols(
        &mut self,
        request: SymbolSearchRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        if self.search_in_flight {
            return Err(RithmicSessionError::RequestInFlight);
        }
        self.send(OutboundRequest::SearchSymbols(request))?;
        self.search_in_flight = true;
        Ok(())
    }

    /// Requests exact metadata for one provider instrument.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn request_instrument_reference(
        &mut self,
        request: InstrumentReferenceRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OutboundRequest::InstrumentReference(request))
    }

    /// Updates one read-only trade/quote/aggregate-book subscription.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn update_market_data(
        &mut self,
        request: MarketDataSubscription<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OutboundRequest::MarketData(request))
    }

    /// Reads and decodes one bounded provider message.
    ///
    /// # Errors
    ///
    /// Returns a redacted deadline, transport, or protocol failure.
    pub fn read_next(&mut self) -> Result<RithmicSessionMessage, RithmicSessionError> {
        let message = self.connection.read_next()?;
        if matches!(
            &message,
            RithmicSessionMessage::Catalog(DecodedCatalogMessage::SearchComplete { .. })
        ) {
            self.search_in_flight = false;
        }
        Ok(message)
    }

    pub(crate) fn read_next_until(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicSessionMessage, RithmicSessionError> {
        let message = self.connection.read_next_until(deadline)?;
        if matches!(
            &message,
            RithmicSessionMessage::Catalog(DecodedCatalogMessage::SearchComplete { .. })
        ) {
            self.search_in_flight = false;
        }
        Ok(message)
    }

    /// Closes the authenticated WebSocket within the configured close deadline.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport failure if the close frame cannot be sent.
    pub fn close(self) -> Result<(), RithmicSessionError> {
        self.connection.close()
    }

    fn send(&mut self, request: OutboundRequest<'_>) -> Result<(), RithmicSessionError> {
        self.connection.send(request)
    }
}

/// Authenticated read-only history-plant connection.
pub struct RithmicHistoryConnection {
    connection: AuthenticatedConnection,
    replay_in_flight: Option<ReplayKind>,
}

impl fmt::Debug for RithmicHistoryConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicHistoryConnection")
            .field("heartbeat_interval", &self.connection.heartbeat_interval)
            .field("limits", &self.connection.limits)
            .finish_non_exhaustive()
    }
}

impl RithmicHistoryConnection {
    const fn new(connection: AuthenticatedConnection) -> Self {
        Self {
            connection,
            replay_in_flight: None,
        }
    }

    #[must_use]
    pub const fn heartbeat_interval(&self) -> Duration {
        self.connection.heartbeat_interval()
    }

    /// Sends a provider heartbeat request.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn send_heartbeat(&mut self) -> Result<(), RithmicSessionError> {
        self.connection.send(OutboundRequest::Heartbeat)
    }

    /// Starts one bounded time-bar replay.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn replay_time_bars(
        &mut self,
        request: TimeBarReplayRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.start_replay(ReplayKind::Time, OutboundRequest::TimeBarReplay(request))
    }

    /// Starts one bounded tick-bar replay.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn replay_tick_bars(
        &mut self,
        request: TickBarReplayRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.start_replay(ReplayKind::Tick, OutboundRequest::TickBarReplay(request))
    }

    /// Reads and decodes one bounded provider message.
    ///
    /// # Errors
    ///
    /// Returns a redacted deadline, transport, or protocol failure.
    pub fn read_next(&mut self) -> Result<RithmicSessionMessage, RithmicSessionError> {
        self.read_next_until(Instant::now() + self.connection.limits.response_timeout)
    }

    pub(crate) fn read_next_until(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicSessionMessage, RithmicSessionError> {
        let message = self.connection.read_next_until(deadline)?;
        if let RithmicSessionMessage::History(DecodedHistoryMessage::ReplayComplete {
            kind, ..
        }) = &message
            && self.replay_in_flight.as_ref() == Some(kind)
        {
            self.replay_in_flight = None;
        }
        Ok(message)
    }

    /// Logs out and closes within the configured close deadline.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn close(self) -> Result<(), RithmicSessionError> {
        self.connection.close()
    }

    fn start_replay(
        &mut self,
        kind: ReplayKind,
        request: OutboundRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        if self.replay_in_flight.is_some() {
            return Err(RithmicSessionError::RequestInFlight);
        }
        self.connection.send(request)?;
        self.replay_in_flight = Some(kind);
        Ok(())
    }
}

fn send_request(
    socket: &mut RithmicWebSocket,
    backend: &RithmicProtocolBackend,
    request: OutboundRequest<'_>,
) -> Result<(), RithmicSessionError> {
    let frame = backend.encode(request).map_err(map_protocol_error)?;
    socket
        .send(Message::Binary(Bytes::from_owner(frame)))
        .map_err(map_websocket_error)
}

fn read_control(
    socket: &mut RithmicWebSocket,
    backend: &RithmicProtocolBackend,
) -> Result<DecodedControlMessage, RithmicSessionError> {
    loop {
        match socket.read().map_err(map_websocket_error)? {
            Message::Binary(frame) => {
                return backend.decode_control(&frame).map_err(map_protocol_error);
            }
            Message::Ping(_) | Message::Pong(_) => {
                socket.flush().map_err(map_websocket_error)?;
            }
            Message::Close(_) | Message::Text(_) | Message::Frame(_) => {
                return Err(RithmicSessionError::UnexpectedMessage);
            }
        }
    }
}

fn read_binary_until(
    socket: &mut RithmicWebSocket,
    deadline: Instant,
) -> Result<Bytes, RithmicSessionError> {
    loop {
        if Instant::now() >= deadline {
            return Err(RithmicSessionError::Deadline);
        }
        match socket.read().map_err(map_websocket_error)? {
            Message::Binary(frame) => return Ok(frame),
            Message::Ping(_) | Message::Pong(_) => {
                socket.flush().map_err(map_websocket_error)?;
            }
            Message::Close(_) | Message::Text(_) | Message::Frame(_) => {
                return Err(RithmicSessionError::UnexpectedMessage);
            }
        }
    }
}

fn decode_session_message(frame: &[u8]) -> Result<RithmicSessionMessage, RithmicSessionError> {
    let codec = RithmicProtocolCodec;
    match codec.decode_control(frame) {
        Ok(message) => return Ok(RithmicSessionMessage::Control(message)),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    match codec.decode_catalog(frame) {
        Ok(message) => return Ok(RithmicSessionMessage::Catalog(message)),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    match codec.decode_market(frame) {
        Ok(message) => return Ok(RithmicSessionMessage::Market(message)),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    codec
        .decode_history(frame)
        .map(RithmicSessionMessage::History)
        .map_err(map_protocol_error)
}

fn finish_discovery_close(
    socket: &mut RithmicWebSocket,
    timeout: Duration,
) -> Result<(), RithmicSessionError> {
    let deadline = Instant::now() + timeout;
    set_deadline(socket, deadline);
    loop {
        if Instant::now() >= deadline {
            return Err(RithmicSessionError::DiscoveryClose);
        }
        match socket.read() {
            Ok(Message::Close(_)) => {
                socket
                    .flush()
                    .map_err(|_| RithmicSessionError::DiscoveryClose)?;
                return Ok(());
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                socket
                    .flush()
                    .map_err(|_| RithmicSessionError::DiscoveryClose)?;
            }
            Err(tungstenite::Error::ConnectionClosed) => return Ok(()),
            Ok(_) | Err(_) => return Err(RithmicSessionError::DiscoveryClose),
        }
    }
}

fn map_protocol_error(error: ProtocolError) -> RithmicSessionError {
    match error {
        ProtocolError::KitUnavailable => RithmicSessionError::KitUnavailable,
        ProtocolError::TemplateVersionMismatch => RithmicSessionError::SchemaMismatch,
        _ => RithmicSessionError::Protocol,
    }
}

fn map_websocket_error(error: tungstenite::Error) -> RithmicSessionError {
    match error {
        tungstenite::Error::Io(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            RithmicSessionError::Cancelled
        }
        tungstenite::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            RithmicSessionError::Deadline
        }
        tungstenite::Error::Capacity(_) => RithmicSessionError::Protocol,
        _ => RithmicSessionError::Transport,
    }
}
