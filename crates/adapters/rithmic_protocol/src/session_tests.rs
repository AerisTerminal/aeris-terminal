#![cfg(rithmic_kit)]

use crate::{
    DecodedControlMessage, DecodedHistoryMessage, DecodedMarketMessage, DecodedTimeBar,
    DecodedTimeBarType, HistorySource, RITHMIC_APPLICATION_NAME, ReplayKind, RetryDisposition,
    RithmicApplication, RithmicCredentials, RithmicSessionError, RithmicSessionLimits,
    RithmicSessionMessage, RithmicTestSession, TimeBarReplayRequest, TimeBarType,
    endpoint::RithmicEndpoint, generated::rti,
};
use prost::Message as _;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};
use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, accept_with_config, protocol::WebSocketConfig};

const FIXTURE_USER: &str = "local-fixture-user";
const FIXTURE_PASSWORD: &str = "local-fixture-password";
const TEST_SYSTEM: &str = "Rithmic Test";
const SERVER_IO_TIMEOUT: Duration = Duration::from_secs(3);

type ServerWebSocket = WebSocket<StreamOwned<ServerConnection, TcpStream>>;

#[test]
fn discovery_closes_before_fresh_ticker_login_over_tls() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-two-connection-fixture".to_string())
        .spawn(move || -> Result<(SocketAddr, SocketAddr), String> {
            let (mut discovery, discovery_peer) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
                .map_err(|error| error.to_string())?;
            discovery.close(None).map_err(|error| error.to_string())?;
            finish_server_close(&mut discovery)?;
            drop(discovery);

            let (mut ticker, ticker_peer) = accept_websocket(&listener, &server_config)?;
            assert_login_request(&read_binary(&mut ticker)?)?;
            ticker
                .send(Message::binary(login_response(true, &[])))
                .map_err(|error| error.to_string())?;
            assert_heartbeat_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(heartbeat_response()))
                .map_err(|error| error.to_string())?;
            ticker
                .send(Message::binary(trade_update()))
                .map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut ticker)?;
            finish_server_close(&mut ticker)?;
            Ok((discovery_peer, ticker_peer))
        })
        .expect("spawn local Rithmic TLS server");

    let mut connection = RithmicTestSession::connect_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        None,
        client_config,
    )
    .expect("discover and log in over local TLS");
    assert_eq!(connection.heartbeat_interval(), Duration::from_secs(10));
    assert_eq!(connection.login_metadata().plant.name(), "ticker");
    assert_eq!(
        connection.login_metadata().unique_user_id.as_deref(),
        Some("fixture-unique-user-id")
    );
    assert!(connection.login_metadata().started_at_utc.ends_with('Z'));
    let debug = format!("{connection:?}");
    assert!(!debug.contains(FIXTURE_USER));
    assert!(!debug.contains(FIXTURE_PASSWORD));
    connection
        .send_heartbeat()
        .expect("send protocol heartbeat");
    assert!(matches!(
        connection.read_next().expect("read heartbeat response"),
        RithmicSessionMessage::Control(DecodedControlMessage::Heartbeat { accepted: true, .. })
    ));
    assert!(matches!(
        connection.read_next().expect("read live trade"),
        RithmicSessionMessage::Market(DecodedMarketMessage::Trade(update))
            if update.identity.symbol == "ESM7" && update.size == 3
    ));
    connection.close().expect("send ticker close frame");

    let (discovery_peer, ticker_peer) = server
        .join()
        .expect("local Rithmic TLS server did not panic")
        .expect("local Rithmic TLS lifecycle completed");
    assert_ne!(discovery_peer, ticker_peer);
}

#[test]
fn priceless_trade_marker_is_skipped_before_the_next_trade() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-trade-marker-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
                .map_err(|error| error.to_string())?;
            discovery.close(None).map_err(|error| error.to_string())?;
            finish_server_close(&mut discovery)?;
            drop(discovery);

            let (mut ticker, _) = accept_websocket(&listener, &server_config)?;
            assert_login_request(&read_binary(&mut ticker)?)?;
            ticker
                .send(Message::binary(login_response(true, &[])))
                .map_err(|error| error.to_string())?;
            ticker
                .send(Message::binary(trade_marker()))
                .map_err(|error| error.to_string())?;
            ticker
                .send(Message::binary(trade_update()))
                .map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut ticker)?;
            finish_server_close(&mut ticker)
        })
        .expect("spawn trade marker fixture");

    let mut connection = RithmicTestSession::connect_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        None,
        client_config,
    )
    .expect("discover and log in over local TLS");
    assert!(matches!(
        connection.read_next().expect("read past the marker"),
        RithmicSessionMessage::Market(DecodedMarketMessage::Trade(update))
            if update.identity.symbol == "ESM7" && update.size == 3
    ));
    connection.close().expect("send ticker close frame");
    server
        .join()
        .expect("trade marker fixture did not panic")
        .expect("trade marker lifecycle completed");
}

#[test]
fn authenticated_close_deadline_survives_continuous_control_frames() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-close-deadline-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
                .map_err(|error| error.to_string())?;
            discovery.close(None).map_err(|error| error.to_string())?;
            finish_server_close(&mut discovery)?;
            drop(discovery);

            let (mut ticker, _) = accept_websocket(&listener, &server_config)?;
            assert_login_request(&read_binary(&mut ticker)?)?;
            ticker
                .send(Message::binary(login_response(true, &[])))
                .map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut ticker)?);
            let flood_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < flood_deadline {
                if ticker.send(Message::Ping(Vec::new().into())).is_err() {
                    break;
                }
            }
            Ok(())
        })
        .expect("spawn close deadline fixture");

    let mut limits = fixture_limits();
    limits.close_timeout = Duration::from_millis(100);
    let connection = RithmicTestSession::connect_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        limits,
        None,
        client_config,
    )
    .expect("discover and log in over local TLS");
    let started = Instant::now();
    assert_eq!(connection.close(), Err(RithmicSessionError::Deadline));
    assert!(started.elapsed() < Duration::from_secs(1));
    server
        .join()
        .expect("close deadline fixture did not panic")
        .expect("close deadline fixture completed");
}

#[test]
fn history_replay_uses_fresh_history_plant_connection_over_tls() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-history-plant-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
                .map_err(|error| error.to_string())?;
            discovery.close(None).map_err(|error| error.to_string())?;
            finish_server_close(&mut discovery)?;
            drop(discovery);

            let (mut history, _) = accept_websocket(&listener, &server_config)?;
            assert_history_login_request(&read_binary(&mut history)?)?;
            history
                .send(Message::binary(login_response(true, &[])))
                .map_err(|error| error.to_string())?;
            assert_time_replay_request(&read_binary(&mut history)?);
            history
                .send(Message::binary(time_replay_bar()))
                .map_err(|error| error.to_string())?;
            history
                .send(Message::binary(time_replay_complete()))
                .map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut history)?);
            history
                .send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut history)?;
            finish_server_close(&mut history)
        })
        .expect("spawn local history TLS server");

    let mut connection = RithmicTestSession::connect_history_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        None,
        client_config,
    )
    .expect("discover and log in to history plant");
    connection
        .replay_time_bars(TimeBarReplayRequest {
            symbol: "ESM7",
            exchange: "CME",
            bar_type: TimeBarType::Minute,
            period: 1,
            start_seconds: 1_800_000_000,
            finish_seconds: 1_800_000_060,
            maximum_bars: 2,
        })
        .expect("send time replay request");
    assert_eq!(
        connection.replay_time_bars(TimeBarReplayRequest {
            symbol: "ESM7",
            exchange: "CME",
            bar_type: TimeBarType::Minute,
            period: 1,
            start_seconds: 1_800_000_000,
            finish_seconds: 1_800_000_060,
            maximum_bars: 2,
        }),
        Err(RithmicSessionError::RequestInFlight)
    );
    assert!(matches!(
        connection.read_next().expect("read replay bar"),
        RithmicSessionMessage::History(DecodedHistoryMessage::TimeBar {
            source: HistorySource::Replay,
            bar: DecodedTimeBar {
                bar_type: DecodedTimeBarType::Minute,
                marker_seconds: 1_800_000_000,
                ..
            },
        })
    ));
    assert_eq!(
        connection.read_next().expect("read replay completion"),
        RithmicSessionMessage::History(DecodedHistoryMessage::ReplayComplete {
            kind: ReplayKind::Time,
            accepted: true,
        })
    );
    connection.close().expect("close history connection");
    server
        .join()
        .expect("local history TLS server did not panic")
        .expect("local history TLS lifecycle completed");
}

#[test]
fn unavailable_test_system_is_terminal_and_redacted() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-terminal-system-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(
                    &["Rithmic 01"],
                    &[FIXTURE_USER, FIXTURE_PASSWORD],
                )))
                .map_err(|error| error.to_string())
        })
        .expect("spawn terminal Rithmic TLS server");

    let result = RithmicTestSession::connect_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        None,
        client_config,
    );
    let Err(error) = result else {
        panic!("unsupported system discovery unexpectedly logged in");
    };
    assert_eq!(error, RithmicSessionError::TestSystemUnavailable);
    assert_eq!(error.retry_disposition(), RetryDisposition::Terminal);
    let output = format!("{error:?} {error}");
    assert!(!output.contains(FIXTURE_USER));
    assert!(!output.contains(FIXTURE_PASSWORD));
    server
        .join()
        .expect("terminal Rithmic TLS server did not panic")
        .expect("terminal Rithmic TLS scenario completed");
}

#[test]
fn discovery_response_deadline_is_bounded_and_transient() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-deadline-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            thread::sleep(Duration::from_millis(300));
            Ok(())
        })
        .expect("spawn deadline Rithmic TLS server");

    let started = Instant::now();
    let result = RithmicTestSession::connect_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        RithmicSessionLimits {
            response_timeout: Duration::from_millis(100),
            ..fixture_limits()
        },
        None,
        client_config,
    );
    assert!(matches!(result, Err(RithmicSessionError::Deadline)));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        RithmicSessionError::Deadline.retry_disposition(),
        RetryDisposition::Transient
    );
    server
        .join()
        .expect("deadline Rithmic TLS server did not panic")
        .expect("deadline Rithmic TLS scenario completed");
}

struct LocalTlsFixture {
    listener: TcpListener,
    endpoint: RithmicEndpoint,
    client_config: ClientConfig,
    server_config: Arc<ServerConfig>,
}

impl LocalTlsFixture {
    fn bind() -> Self {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(["localhost".to_string()])
                .expect("generate local TLS certificate");
        let certificate = cert.der().clone();
        let private_key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
        let server_config = ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("select local server TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], private_key)
        .expect("configure local TLS certificate");
        let mut roots = RootCertStore::empty();
        roots.add(certificate).expect("trust local TLS certificate");
        let client_config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("select local client TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local TLS listener");
        let port = listener
            .local_addr()
            .expect("read local TLS address")
            .port();
        let url = Box::leak(format!("wss://localhost:{port}").into_boxed_str());
        let endpoint = RithmicEndpoint::loopback(url, port);
        Self {
            listener,
            endpoint,
            client_config,
            server_config: Arc::new(server_config),
        }
    }
}

fn accept_websocket(
    listener: &TcpListener,
    server_config: &Arc<ServerConfig>,
) -> Result<(ServerWebSocket, SocketAddr), String> {
    let (stream, peer) = listener.accept().map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(SERVER_IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(SERVER_IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let connection =
        ServerConnection::new(Arc::clone(server_config)).map_err(|error| error.to_string())?;
    let tls = StreamOwned::new(connection, stream);
    let config = WebSocketConfig::default()
        .read_buffer_size(64 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * 1024 * 1024)
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    accept_with_config(tls, Some(config))
        .map(|socket| (socket, peer))
        .map_err(|error| error.to_string())
}

fn read_binary(socket: &mut ServerWebSocket) -> Result<Vec<u8>, String> {
    loop {
        match socket.read().map_err(|error| error.to_string())? {
            Message::Binary(frame) => return Ok(frame.to_vec()),
            Message::Ping(_) | Message::Pong(_) => {
                socket.flush().map_err(|error| error.to_string())?;
            }
            Message::Text(_) | Message::Close(_) | Message::Frame(_) => {
                return Err("expected one binary Rithmic request".to_string());
            }
        }
    }
}

fn require_close(socket: &mut ServerWebSocket) -> Result<(), String> {
    loop {
        match socket.read().map_err(|error| error.to_string())? {
            Message::Close(_) => return Ok(()),
            Message::Ping(_) | Message::Pong(_) => {
                socket.flush().map_err(|error| error.to_string())?;
            }
            Message::Binary(_) | Message::Text(_) | Message::Frame(_) => {
                return Err("expected WebSocket close frame".to_string());
            }
        }
    }
}

fn finish_server_close(socket: &mut ServerWebSocket) -> Result<(), String> {
    match socket.flush() {
        Ok(()) | Err(tungstenite::Error::ConnectionClosed) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn assert_system_discovery_request(frame: &[u8]) {
    let request =
        rti::RequestRithmicSystemInfo::decode(frame).expect("decode system discovery request");
    assert_eq!(request.template_id, 16);
    assert!(request.user_msg.is_empty());
}

fn assert_login_request(frame: &[u8]) -> Result<(), String> {
    let request = rti::RequestLogin::decode(frame).map_err(|error| error.to_string())?;
    if request.template_id != 10
        || request.template_version.as_deref() != Some(env!("RITHMIC_TEMPLATE_VERSION"))
        || request.user.as_deref() != Some(FIXTURE_USER)
        || request.password.as_deref() != Some(FIXTURE_PASSWORD)
        || request.app_name.as_deref() != Some(RITHMIC_APPLICATION_NAME)
        || request.app_version.as_deref() != Some("0.1.0")
        || request.system_name.as_deref() != Some(TEST_SYSTEM)
        || request.infra_type != Some(rti::request_login::SysInfraType::TickerPlant.into())
        || request.aggregated_quotes != Some(false)
    {
        return Err("ticker login request did not match the bounded fixture".to_string());
    }
    Ok(())
}

fn assert_history_login_request(frame: &[u8]) -> Result<(), String> {
    let request = rti::RequestLogin::decode(frame).map_err(|error| error.to_string())?;
    if request.template_id != 10
        || request.template_version.as_deref() != Some(env!("RITHMIC_TEMPLATE_VERSION"))
        || request.user.as_deref() != Some(FIXTURE_USER)
        || request.password.as_deref() != Some(FIXTURE_PASSWORD)
        || request.app_name.as_deref() != Some(RITHMIC_APPLICATION_NAME)
        || request.app_version.as_deref() != Some("0.1.0")
        || request.system_name.as_deref() != Some(TEST_SYSTEM)
        || request.infra_type != Some(rti::request_login::SysInfraType::HistoryPlant.into())
        || request.aggregated_quotes.is_some()
    {
        return Err("history login request did not match the bounded fixture".to_string());
    }
    Ok(())
}

fn assert_time_replay_request(frame: &[u8]) {
    let request = rti::RequestTimeBarReplay::decode(frame).expect("decode time replay request");
    assert_eq!(request.template_id, 202);
    assert_eq!(request.symbol.as_deref(), Some("ESM7"));
    assert_eq!(request.exchange.as_deref(), Some("CME"));
    assert_eq!(
        request.bar_type,
        Some(rti::request_time_bar_replay::BarType::MinuteBar.into())
    );
    assert_eq!(request.bar_type_period, Some(1));
    assert_eq!(request.start_index, Some(1_800_000_000));
    assert_eq!(request.finish_index, Some(1_800_000_060));
    assert_eq!(request.user_max_count, Some(2));
}

fn assert_logout_request(frame: &[u8]) {
    let request = rti::RequestLogout::decode(frame).expect("decode logout request");
    assert_eq!(request.template_id, 12);
    assert!(request.user_msg.is_empty());
}

fn assert_heartbeat_request(frame: &[u8]) {
    let request = rti::RequestHeartbeat::decode(frame).expect("decode heartbeat request");
    assert_eq!(request.template_id, 18);
    assert!(request.user_msg.is_empty());
    assert_eq!(request.ssboe, None);
    assert_eq!(request.usecs, None);
}

fn system_info_response(systems: &[&str], user_messages: &[&str]) -> Vec<u8> {
    rti::ResponseRithmicSystemInfo {
        template_id: 17,
        user_msg: user_messages
            .iter()
            .map(|message| (*message).to_string())
            .collect(),
        rp_code: vec!["0".to_string()],
        system_name: systems.iter().map(|system| (*system).to_string()).collect(),
        has_aggregated_quotes: vec![false; systems.len()],
    }
    .encode_to_vec()
}

fn login_response(accepted: bool, user_messages: &[&str]) -> Vec<u8> {
    rti::ResponseLogin {
        template_id: 11,
        template_version: Some(env!("RITHMIC_TEMPLATE_VERSION").to_string()),
        user_msg: user_messages
            .iter()
            .map(|message| (*message).to_string())
            .collect(),
        rp_code: if accepted {
            vec!["0".to_string()]
        } else {
            vec!["1".to_string(), "rejected".to_string()]
        },
        fcm_id: None,
        ib_id: None,
        country_code: None,
        state_code: None,
        unique_user_id: Some("fixture-unique-user-id".to_string()),
        heartbeat_interval: Some(10.0),
    }
    .encode_to_vec()
}

fn logout_response() -> Vec<u8> {
    rti::ResponseLogout {
        template_id: 13,
        user_msg: Vec::new(),
        rp_code: vec!["0".to_string()],
    }
    .encode_to_vec()
}

fn heartbeat_response() -> Vec<u8> {
    rti::ResponseHeartbeat {
        template_id: 19,
        user_msg: Vec::new(),
        rp_code: vec!["0".to_string()],
        ssboe: Some(1_800_000_000),
        usecs: Some(123_456),
    }
    .encode_to_vec()
}

fn trade_update() -> Vec<u8> {
    rti::LastTrade {
        template_id: 150,
        symbol: Some("ESM7".to_string()),
        exchange: Some("CME".to_string()),
        presence_bits: Some(1),
        clear_bits: Some(0),
        is_snapshot: Some(false),
        trade_price: Some(5_100.25),
        trade_size: Some(3),
        aggressor: Some(rti::last_trade::TransactionType::Buy.into()),
        exchange_order_id: None,
        aggressor_exchange_order_id: None,
        net_change: None,
        percent_change: None,
        volume: None,
        vwap: None,
        trade_time: None,
        ssboe: Some(1_800_000_000),
        usecs: Some(123_457),
        source_ssboe: None,
        source_usecs: None,
        source_nsecs: None,
        jop_ssboe: None,
        jop_nsecs: None,
    }
    .encode_to_vec()
}

fn trade_marker() -> Vec<u8> {
    // Mirrors the live plant's session/clear marker: presence and clear bits
    // set, but no price, size, or aggressor. It must be skipped, never a
    // trade and never a stream failure.
    rti::LastTrade {
        template_id: 150,
        symbol: Some("ESM7".to_string()),
        exchange: Some("CME".to_string()),
        presence_bits: Some(1),
        clear_bits: Some(1),
        is_snapshot: Some(false),
        trade_price: None,
        trade_size: None,
        aggressor: None,
        exchange_order_id: None,
        aggressor_exchange_order_id: None,
        net_change: None,
        percent_change: None,
        volume: None,
        vwap: None,
        trade_time: None,
        ssboe: Some(1_800_000_000),
        usecs: Some(123_457),
        source_ssboe: None,
        source_usecs: None,
        source_nsecs: None,
        jop_ssboe: None,
        jop_nsecs: None,
    }
    .encode_to_vec()
}

fn time_replay_bar() -> Vec<u8> {
    rti::ResponseTimeBarReplay {
        template_id: 203,
        rq_handler_rp_code: vec!["0".to_string()],
        symbol: Some("ESM7".to_string()),
        exchange: Some("CME".to_string()),
        r#type: Some(rti::response_time_bar_replay::BarType::MinuteBar.into()),
        period: Some("1".to_string()),
        marker: Some(1_800_000_000),
        num_trades: Some(10),
        volume: Some(20),
        bid_volume: Some(8),
        ask_volume: Some(12),
        open_price: Some(5_100.0),
        close_price: Some(5_101.0),
        high_price: Some(5_102.0),
        low_price: Some(5_099.0),
        ..Default::default()
    }
    .encode_to_vec()
}

fn time_replay_complete() -> Vec<u8> {
    rti::ResponseTimeBarReplay {
        template_id: 203,
        rp_code: vec!["0".to_string()],
        ..Default::default()
    }
    .encode_to_vec()
}

const fn fixture_credentials() -> RithmicCredentials<'static> {
    RithmicCredentials {
        user: FIXTURE_USER,
        password: FIXTURE_PASSWORD,
    }
}

const fn fixture_application() -> RithmicApplication<'static> {
    RithmicApplication {
        name: RITHMIC_APPLICATION_NAME,
        version: "0.1.0",
    }
}

fn fixture_limits() -> RithmicSessionLimits {
    RithmicSessionLimits {
        connect_timeout: Duration::from_secs(3),
        handshake_timeout: Duration::from_secs(3),
        response_timeout: Duration::from_secs(3),
        close_timeout: Duration::from_secs(3),
        maximum_message_bytes: 1024 * 1024,
        maximum_write_buffer_bytes: 2 * 1024 * 1024,
    }
}
