use crate::{
    DecodedCatalogMessage, DecodedControlMessage, DecodedHistoryMessage, DecodedMarketMessage,
    DepthByOrderSnapshotRequest, DepthByOrderSubscription, InstrumentReferenceRequest,
    LoginRequest, MarketDataSubscription, OutboundRequest, ProtocolError, ReplayKind,
    RithmicOrderConnection, RithmicPlant, RithmicPnlConnection, RithmicProtocolBackend,
    RithmicProtocolCodec, RithmicSessionError, RithmicSessionLimits, SymbolSearchRequest,
    TickBarReplayRequest, TimeBarReplayRequest,
    endpoint::RithmicEndpoint,
    network::{
        ConnectionAbort, RithmicWebSocket, begin_shutdown, connect_websocket, default_tls_config,
        is_cancellation, set_deadline, stop_requested,
    },
};
use aeris_observability::diagnostic;
use chrono::{DateTime, SecondsFormat, Utc};
use rustls::ClientConfig;
use std::{
    fmt,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime},
};
use tungstenite::{Bytes, Message};

const TEST_SYSTEM: &str = "Rithmic Test";

/// Logout budget once the session owner has requested a stop. Shutdown paths
/// must finish within the desktop's shutdown budget, so a stopping session
/// waits at most this long for the provider's logout acknowledgement even when
/// the configured `close_timeout` is longer.
pub(crate) const STOPPING_LOGOUT_TIMEOUT: Duration = Duration::from_secs(1);

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

/// Provider-issued identity and UTC start time for one authenticated plant
/// connection. Rithmic support uses this metadata to find a reviewed session
/// in its server logs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicLoginMetadata {
    pub plant: RithmicPlant,
    pub system_name: &'static str,
    pub unique_user_id: Option<String>,
    pub started_at_utc: String,
}

/// One sanitized inbound message from an authenticated ticker or history session.
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
            RithmicPlant::History,
        )
        .map(RithmicHistoryConnection::new)
    }

    /// Performs system discovery, then logs in to the order plant on a fresh
    /// WSS connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted terminal or transient session failure.
    pub fn discover_and_login_orders(
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Option<Arc<AtomicBool>>,
    ) -> Result<RithmicOrderConnection, RithmicSessionError> {
        let tls_config = default_tls_config()?;
        Self::connect_plant_with(
            RithmicEndpoint::TEST,
            credentials,
            application,
            limits,
            ConnectionControl { stop, abort: None },
            tls_config,
            RithmicPlant::Order,
        )
        .map(RithmicOrderConnection::new)
    }

    /// Performs system discovery, then logs in to the P&L plant on a fresh WSS
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted terminal or transient session failure.
    pub fn discover_and_login_pnl(
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Option<Arc<AtomicBool>>,
    ) -> Result<RithmicPnlConnection, RithmicSessionError> {
        let tls_config = default_tls_config()?;
        Self::connect_plant_with(
            RithmicEndpoint::TEST,
            credentials,
            application,
            limits,
            ConnectionControl { stop, abort: None },
            tls_config,
            RithmicPlant::Pnl,
        )
        .map(RithmicPnlConnection::new)
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
            RithmicPlant::Ticker,
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
            RithmicPlant::Ticker,
        )
        .map(RithmicTickerConnection::new)
    }

    #[cfg(all(test, rithmic_kit))]
    pub(crate) fn connect_with_abort(
        endpoint: RithmicEndpoint,
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        stop: Arc<AtomicBool>,
        abort: &ConnectionAbort,
        tls_config: ClientConfig,
    ) -> Result<RithmicTickerConnection, RithmicSessionError> {
        Self::connect_plant_with(
            endpoint,
            credentials,
            application,
            limits,
            ConnectionControl {
                stop: Some(stop),
                abort: Some(abort),
            },
            tls_config,
            RithmicPlant::Ticker,
        )
        .map(RithmicTickerConnection::new)
    }

    #[cfg(all(test, rithmic_kit))]
    pub(crate) fn connect_order_with(
        endpoint: RithmicEndpoint,
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        tls_config: ClientConfig,
    ) -> Result<RithmicOrderConnection, RithmicSessionError> {
        Self::connect_plant_with(
            endpoint,
            credentials,
            application,
            limits,
            ConnectionControl {
                stop: None,
                abort: None,
            },
            tls_config,
            RithmicPlant::Order,
        )
        .map(RithmicOrderConnection::new)
    }

    #[cfg(all(test, rithmic_kit))]
    pub(crate) fn connect_pnl_with(
        endpoint: RithmicEndpoint,
        credentials: RithmicCredentials<'_>,
        application: RithmicApplication<'_>,
        limits: RithmicSessionLimits,
        tls_config: ClientConfig,
    ) -> Result<RithmicPnlConnection, RithmicSessionError> {
        Self::connect_plant_with(
            endpoint,
            credentials,
            application,
            limits,
            ConnectionControl {
                stop: None,
                abort: None,
            },
            tls_config,
            RithmicPlant::Pnl,
        )
        .map(RithmicPnlConnection::new)
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
            RithmicPlant::History,
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
        plant: RithmicPlant,
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
                unique_user_id,
            } => {
                let heartbeat_interval = Duration::from_secs_f64(heartbeat_seconds);
                if heartbeat_interval.is_zero() {
                    return Err(RithmicSessionError::Protocol);
                }
                let metadata = RithmicLoginMetadata {
                    plant,
                    system_name: TEST_SYSTEM,
                    unique_user_id,
                    started_at_utc: utc_timestamp(SystemTime::now()),
                };
                log_session_event(&session_event_record("login", &metadata, None));
                Ok(AuthenticatedConnection {
                    socket,
                    heartbeat_interval,
                    limits,
                    metadata,
                    logout_confirmed: false,
                })
            }
            DecodedControlMessage::Login {
                accepted: false, ..
            } => Err(RithmicSessionError::LoginRejected),
            _ => Err(RithmicSessionError::UnexpectedMessage),
        }
    }
}

pub(crate) struct AuthenticatedConnection {
    socket: RithmicWebSocket,
    heartbeat_interval: Duration,
    limits: RithmicSessionLimits,
    metadata: RithmicLoginMetadata,
    logout_confirmed: bool,
}

impl AuthenticatedConnection {
    pub(crate) const fn heartbeat_interval(&self) -> Duration {
        self.heartbeat_interval
    }

    pub(crate) const fn limits(&self) -> RithmicSessionLimits {
        self.limits
    }

    pub(crate) const fn login_metadata(&self) -> &RithmicLoginMetadata {
        &self.metadata
    }

    pub(crate) fn send(&mut self, request: OutboundRequest<'_>) -> Result<(), RithmicSessionError> {
        if !request.permitted_on(self.metadata.plant) {
            return Err(RithmicSessionError::RequestNotPermitted);
        }
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
        loop {
            set_deadline(&mut self.socket, deadline);
            let frame = read_binary_until(&mut self.socket, deadline)?;
            if let Some(message) = decode_session_message(&frame)? {
                return Ok(message);
            }
        }
    }

    /// Reads one binary provider frame for a plant-specific decoder.
    pub(crate) fn read_frame_until(
        &mut self,
        deadline: Instant,
    ) -> Result<Bytes, RithmicSessionError> {
        set_deadline(&mut self.socket, deadline);
        read_binary_until(&mut self.socket, deadline)
    }

    /// Logs out, waits for the provider acknowledgement, then sends the
    /// WebSocket close frame. A session whose owner requested a stop waits at
    /// most [`STOPPING_LOGOUT_TIMEOUT`] for the acknowledgement.
    pub(crate) fn close(mut self) -> Result<(), RithmicSessionError> {
        let timeout = if stop_requested(&self.socket) {
            self.limits.close_timeout.min(STOPPING_LOGOUT_TIMEOUT)
        } else {
            self.limits.close_timeout
        };
        let deadline = Instant::now() + timeout;
        begin_shutdown(&mut self.socket, deadline);
        let backend = RithmicProtocolBackend::detected();
        send_request(&mut self.socket, &backend, OutboundRequest::Logout)?;
        // A streaming plant keeps delivering market frames until the logout is
        // processed, so frames are drained until the acknowledgement or the
        // deadline rather than for a fixed frame count.
        loop {
            let frame = read_binary_until(&mut self.socket, deadline)?;
            let control = match backend.decode_control(&frame) {
                Ok(control) => control,
                Err(ProtocolError::UnsupportedTemplate(_)) => continue,
                Err(error) => return Err(map_protocol_error(error)),
            };
            match control {
                DecodedControlMessage::Logout { accepted: true }
                | DecodedControlMessage::ForcedLogout => break,
                DecodedControlMessage::Reject
                | DecodedControlMessage::Logout { accepted: false } => {
                    return Err(RithmicSessionError::Protocol);
                }
                _ => {}
            }
        }
        self.logout_confirmed = true;
        self.socket
            .close(None)
            .map_err(|_| RithmicSessionError::Transport)
    }
}

impl Drop for AuthenticatedConnection {
    fn drop(&mut self) {
        let ended_at_utc = utc_timestamp(SystemTime::now());
        log_session_event(&session_event_record(
            "end",
            &self.metadata,
            Some(SessionEnd {
                ended_at_utc: &ended_at_utc,
                logout_confirmed: self.logout_confirmed,
            }),
        ));
    }
}

fn utc_timestamp(timestamp: SystemTime) -> String {
    DateTime::<Utc>::from(timestamp).to_rfc3339_opts(SecondsFormat::Millis, true)
}

struct SessionEnd<'a> {
    ended_at_utc: &'a str,
    logout_confirmed: bool,
}

fn session_event_record(
    event: &str,
    metadata: &RithmicLoginMetadata,
    end: Option<SessionEnd<'_>>,
) -> serde_json::Value {
    let mut record = serde_json::json!({
        "event": event,
        "system": metadata.system_name,
        "plant": metadata.plant.name(),
        "unique_user_id": metadata.unique_user_id,
        "started_at_utc": metadata.started_at_utc,
        "ended_at_utc": end.as_ref().map(|end| end.ended_at_utc),
    });
    if let (Some(end), Some(fields)) = (end, record.as_object_mut()) {
        let logout = if end.logout_confirmed {
            "confirmed"
        } else {
            "unconfirmed"
        };
        fields.insert("logout".to_string(), logout.into());
    }
    record
}

fn log_session_event(record: &serde_json::Value) {
    diagnostic!("AERIS_RITHMIC_SESSION {record}");
}

/// Authenticated ticker-plant connection.
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

    /// Returns the provider identity and UTC start time for this ticker login.
    #[must_use]
    pub const fn login_metadata(&self) -> &RithmicLoginMetadata {
        self.connection.login_metadata()
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

    /// Updates one read-only depth-by-order subscription.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn update_depth_by_order(
        &mut self,
        request: DepthByOrderSubscription<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OutboundRequest::DepthByOrder(request))
    }

    /// Requests one covering read-only depth-by-order image.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn request_depth_by_order_snapshot(
        &mut self,
        request: DepthByOrderSnapshotRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OutboundRequest::DepthByOrderSnapshot(request))
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

/// Authenticated history-plant connection.
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

    /// Returns the provider identity and UTC start time for this history login.
    #[must_use]
    pub const fn login_metadata(&self) -> &RithmicLoginMetadata {
        self.connection.login_metadata()
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

fn decode_session_message(
    frame: &[u8],
) -> Result<Option<RithmicSessionMessage>, RithmicSessionError> {
    let codec = RithmicProtocolCodec;
    match codec.decode_control(frame) {
        Ok(message) => return Ok(Some(RithmicSessionMessage::Control(message))),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    match codec.decode_catalog(frame) {
        Ok(message) => return Ok(Some(RithmicSessionMessage::Catalog(message))),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    match codec.decode_market(frame) {
        Ok(Some(message)) => return Ok(Some(RithmicSessionMessage::Market(message))),
        Ok(None) => return Ok(None),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    codec
        .decode_history(frame)
        .map(|message| Some(RithmicSessionMessage::History(message)))
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

pub(crate) fn map_protocol_error(error: ProtocolError) -> RithmicSessionError {
    diagnostic!("Rithmic protocol category: {error:?}");
    match error {
        ProtocolError::KitUnavailable => RithmicSessionError::KitUnavailable,
        ProtocolError::TemplateVersionMismatch => RithmicSessionError::SchemaMismatch,
        _ => RithmicSessionError::Protocol,
    }
}

fn map_websocket_error(error: tungstenite::Error) -> RithmicSessionError {
    match error {
        tungstenite::Error::Io(error) if is_cancellation(&error) => RithmicSessionError::Cancelled,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> RithmicLoginMetadata {
        RithmicLoginMetadata {
            plant: RithmicPlant::Ticker,
            system_name: TEST_SYSTEM,
            unique_user_id: Some("fixture-unique-user-id".to_string()),
            started_at_utc: "2026-01-02T03:04:05.006Z".to_string(),
        }
    }

    #[test]
    fn session_end_record_distinguishes_confirmed_from_unconfirmed_logout() {
        let login = session_event_record("login", &metadata(), None);
        assert_eq!(
            login,
            serde_json::json!({
                "event": "login",
                "system": TEST_SYSTEM,
                "plant": "ticker",
                "unique_user_id": "fixture-unique-user-id",
                "started_at_utc": "2026-01-02T03:04:05.006Z",
                "ended_at_utc": null,
            })
        );
        for (logout_confirmed, logout) in [(true, "confirmed"), (false, "unconfirmed")] {
            let end = session_event_record(
                "end",
                &metadata(),
                Some(SessionEnd {
                    ended_at_utc: "2026-01-02T03:05:00.000Z",
                    logout_confirmed,
                }),
            );
            assert_eq!(
                end,
                serde_json::json!({
                    "event": "end",
                    "system": TEST_SYSTEM,
                    "plant": "ticker",
                    "unique_user_id": "fixture-unique-user-id",
                    "started_at_utc": "2026-01-02T03:04:05.006Z",
                    "ended_at_utc": "2026-01-02T03:05:00.000Z",
                    "logout": logout,
                })
            );
        }
    }
}
