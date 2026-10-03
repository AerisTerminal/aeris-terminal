#![cfg(rithmic_kit)]

use crate::{
    DecodedControlMessage, DecodedHistoryMessage, DecodedMarketMessage, DecodedOrderMessage,
    DecodedPnlMessage, DecodedTimeBar, DecodedTimeBarType, HistorySource, ProviderSessionDriver,
    ProviderSessionEvent, RITHMIC_APPLICATION_NAME, ReplayKind, RetryDisposition,
    RithmicAccountKey, RithmicApplication, RithmicCallbackLimits, RithmicCredentialBytes,
    RithmicCredentials, RithmicOrderPlantMessage, RithmicPnlPlantMessage, RithmicProviderConfig,
    RithmicProviderDriver, RithmicProviderEvents, RithmicRequestKind, RithmicRequestOutcome,
    RithmicSessionError, RithmicSessionLimits, RithmicSessionMessage, RithmicTestSession,
    SessionGeneration, TimeBarReplayRequest, TimeBarType, endpoint::RithmicEndpoint,
    generated::rti, session::STOPPING_LOGOUT_TIMEOUT,
};
use prost::Message as _;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};
use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::sync_channel,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, accept_with_config, protocol::WebSocketConfig};

const FIXTURE_USER: &str = "local-fixture-user";
const FIXTURE_PASSWORD: &str = "local-fixture-password";
const TEST_SYSTEM: &str = "Rithmic Test";
const SERVER_IO_TIMEOUT: Duration = Duration::from_secs(3);
/// Budget `MarketService::shutdown` receives from the desktop on quit.
const DESKTOP_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);
const FIXTURE_ACCOUNT: RithmicAccountKey<'static> = RithmicAccountKey {
    fcm_id: "fixture-fcm",
    ib_id: "fixture-ib",
    account_id: "fixture-account",
};

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
fn stop_interrupts_a_blocked_tls_read_and_still_logs_out() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-stop-read-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let mut ticker = accept_ticker_login(&listener, &server_config)?;
            assert_logout_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut ticker)?;
            finish_server_close(&mut ticker)
        })
        .expect("spawn stop read fixture");

    let stop = Arc::new(AtomicBool::new(false));
    let mut connection = RithmicTestSession::connect_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        Some(Arc::clone(&stop)),
        client_config,
    )
    .expect("discover and log in over local TLS");
    let stopper_flag = Arc::clone(&stop);
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        stopper_flag.store(true, Ordering::Release);
    });
    let started = Instant::now();
    assert_eq!(
        connection.read_next_until(started + Duration::from_secs(5)),
        Err(RithmicSessionError::Cancelled)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    stopper.join().expect("stopper thread joins");
    connection
        .close()
        .expect("a stopped session still logs out and closes");
    server
        .join()
        .expect("stop read fixture did not panic")
        .expect("stop read lifecycle completed");
}

#[test]
fn driver_stop_logs_out_a_streaming_ticker_session_before_closing() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let (ready_tx, ready_rx) = sync_channel(1);
    let server = thread::Builder::new()
        .name("rithmic-driver-logout-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let mut ticker = accept_ticker_login(&listener, &server_config)?;
            assert_heartbeat_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(heartbeat_response()))
                .map_err(|error| error.to_string())?;
            ready_tx.send(()).map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut ticker)?;
            finish_server_close(&mut ticker)
        })
        .expect("spawn driver logout fixture");

    let (mut driver, events) = fixture_driver(endpoint, client_config);
    let credentials = fixture_credential_bytes();
    driver
        .start_session(session_generation(1), credentials.as_bytes())
        .expect("ticker session starts");
    wait_for_instruments(&events);
    ready_rx
        .recv_timeout(SERVER_IO_TIMEOUT)
        .expect("fixture answers the first heartbeat");

    let started = Instant::now();
    driver
        .stop_session(session_generation(1))
        .expect("streaming session stops");
    // The acknowledged logout returns well before the logout bound, so the
    // driver never needed its abort fallback.
    assert!(started.elapsed() < STOPPING_LOGOUT_TIMEOUT);
    server
        .join()
        .expect("driver logout fixture did not panic")
        .expect("driver logout lifecycle completed");
}

#[test]
fn driver_stop_is_bounded_when_logout_is_never_acknowledged() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let (ready_tx, ready_rx) = sync_channel(1);
    let server = thread::Builder::new()
        .name("rithmic-driver-silent-logout-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let mut ticker = accept_ticker_login(&listener, &server_config)?;
            assert_heartbeat_request(&read_binary(&mut ticker)?);
            ticker
                .send(Message::binary(heartbeat_response()))
                .map_err(|error| error.to_string())?;
            ready_tx.send(()).map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut ticker)?);
            // Never acknowledge; wait for the client to drop the connection.
            while ticker.read().is_ok() {}
            Ok(())
        })
        .expect("spawn silent logout fixture");

    let (mut driver, events) = fixture_driver(endpoint, client_config);
    let credentials = fixture_credential_bytes();
    driver
        .start_session(session_generation(1), credentials.as_bytes())
        .expect("ticker session starts");
    wait_for_instruments(&events);
    ready_rx
        .recv_timeout(SERVER_IO_TIMEOUT)
        .expect("fixture answers the first heartbeat");

    let started = Instant::now();
    driver
        .stop_session(session_generation(1))
        .expect("unacknowledged session still stops");
    let elapsed = started.elapsed();
    assert!(elapsed >= STOPPING_LOGOUT_TIMEOUT);
    assert!(elapsed < DESKTOP_SHUTDOWN_BUDGET);
    server
        .join()
        .expect("silent logout fixture did not panic")
        .expect("silent logout lifecycle completed");
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
fn order_stream_subscribes_before_snapshot_over_tls() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-order-plant-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
                .map_err(|error| error.to_string())?;
            discovery.close(None).map_err(|error| error.to_string())?;
            finish_server_close(&mut discovery)?;
            drop(discovery);

            let (mut order, _) = accept_websocket(&listener, &server_config)?;
            assert_trading_login_request(
                &read_binary(&mut order)?,
                rti::request_login::SysInfraType::OrderPlant,
            )?;
            order
                .send(Message::binary(login_response(true, &[])))
                .map_err(|error| error.to_string())?;
            let subscribe =
                rti::RequestSubscribeForOrderUpdates::decode(read_binary(&mut order)?.as_slice())
                    .map_err(|error| error.to_string())?;
            if subscribe.template_id != 308
                || subscribe.account_id.as_deref() != Some(FIXTURE_ACCOUNT.account_id)
            {
                return Err("expected the order-update subscription first".to_string());
            }
            order
                .send(Message::binary(order_notification(None)))
                .map_err(|error| error.to_string())?;
            order
                .send(Message::binary(order_updates_response()))
                .map_err(|error| error.to_string())?;
            let snapshot = rti::RequestShowOrders::decode(read_binary(&mut order)?.as_slice())
                .map_err(|error| error.to_string())?;
            if snapshot.template_id != 320
                || snapshot.account_id.as_deref() != Some(FIXTURE_ACCOUNT.account_id)
            {
                return Err("expected the order snapshot after the acknowledgement".to_string());
            }
            order
                .send(Message::binary(order_notification(Some(true))))
                .map_err(|error| error.to_string())?;
            order
                .send(Message::binary(show_orders_response()))
                .map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut order)?);
            order
                .send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut order)?;
            finish_server_close(&mut order)
        })
        .expect("spawn local order-plant TLS server");

    let mut connection = RithmicTestSession::connect_order_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        client_config,
    )
    .expect("discover and log in to the order plant");
    assert_eq!(connection.login_metadata().plant.name(), "order");
    assert_eq!(
        connection.request_order_snapshot(FIXTURE_ACCOUNT),
        Err(RithmicSessionError::RequestNotPermitted)
    );
    connection
        .start_order_stream(FIXTURE_ACCOUNT, Instant::now() + Duration::from_secs(3))
        .expect("subscribe, then request the snapshot");
    assert!(matches!(
        order_payload(connection.read_next().expect("read the retained live update")),
        DecodedOrderMessage::OrderNotification(update) if !update.order.is_snapshot
    ));
    assert!(matches!(
        order_payload(connection.read_next().expect("read the snapshot row")),
        DecodedOrderMessage::OrderNotification(update) if update.order.is_snapshot
    ));
    assert!(matches!(
        order_payload(connection.read_next().expect("read the snapshot completion")),
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::ShowOrders
                && completion.outcome.is_accepted()
    ));
    connection.close().expect("close order-plant connection");
    server
        .join()
        .expect("local order-plant TLS server did not panic")
        .expect("local order-plant TLS lifecycle completed");
}

#[test]
fn pnl_stream_subscribes_before_snapshot_over_tls() {
    let fixture = LocalTlsFixture::bind();
    let endpoint = fixture.endpoint;
    let client_config = fixture.client_config;
    let listener = fixture.listener;
    let server_config = fixture.server_config;
    let server = thread::Builder::new()
        .name("rithmic-pnl-plant-fixture".to_string())
        .spawn(move || -> Result<(), String> {
            let (mut discovery, _) = accept_websocket(&listener, &server_config)?;
            assert_system_discovery_request(&read_binary(&mut discovery)?);
            discovery
                .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
                .map_err(|error| error.to_string())?;
            discovery.close(None).map_err(|error| error.to_string())?;
            finish_server_close(&mut discovery)?;
            drop(discovery);

            let (mut pnl, _) = accept_websocket(&listener, &server_config)?;
            assert_trading_login_request(
                &read_binary(&mut pnl)?,
                rti::request_login::SysInfraType::PnlPlant,
            )?;
            pnl.send(Message::binary(login_response(true, &[])))
                .map_err(|error| error.to_string())?;
            let subscribe =
                rti::RequestPnLPositionUpdates::decode(read_binary(&mut pnl)?.as_slice())
                    .map_err(|error| error.to_string())?;
            if subscribe.template_id != 400
                || subscribe.request
                    != Some(rti::request_pn_l_position_updates::Request::Subscribe.into())
            {
                return Err("expected the PnL subscription first".to_string());
            }
            pnl.send(Message::binary(pnl_updates_response()))
                .map_err(|error| error.to_string())?;
            let snapshot =
                rti::RequestPnLPositionSnapshot::decode(read_binary(&mut pnl)?.as_slice())
                    .map_err(|error| error.to_string())?;
            if snapshot.template_id != 402 {
                return Err("expected the PnL snapshot after the acknowledgement".to_string());
            }
            pnl.send(Message::binary(account_pnl_snapshot()))
                .map_err(|error| error.to_string())?;
            pnl.send(Message::binary(pnl_snapshot_response()))
                .map_err(|error| error.to_string())?;
            assert_logout_request(&read_binary(&mut pnl)?);
            pnl.send(Message::binary(logout_response()))
                .map_err(|error| error.to_string())?;
            require_close(&mut pnl)?;
            finish_server_close(&mut pnl)
        })
        .expect("spawn local PnL-plant TLS server");

    let mut connection = RithmicTestSession::connect_pnl_with(
        endpoint,
        fixture_credentials(),
        fixture_application(),
        fixture_limits(),
        client_config,
    )
    .expect("discover and log in to the PnL plant");
    assert_eq!(connection.login_metadata().plant.name(), "pnl");
    assert_eq!(
        connection.request_position_snapshot(FIXTURE_ACCOUNT),
        Err(RithmicSessionError::RequestNotPermitted)
    );
    connection
        .start_pnl_stream(FIXTURE_ACCOUNT, Instant::now() + Duration::from_secs(3))
        .expect("subscribe, then request the snapshot");
    assert!(matches!(
        pnl_payload(connection.read_next().expect("read the account snapshot")),
        DecodedPnlMessage::Account(account)
            if account.is_snapshot
                && account.account_balance.map(|value| (value.units(), value.scale()))
                    == Some((5_000_025, 2))
    ));
    assert_eq!(
        pnl_payload(
            connection
                .read_next()
                .expect("read the snapshot completion")
        ),
        DecodedPnlMessage::RequestComplete {
            request: RithmicRequestKind::PnlSnapshot,
            outcome: RithmicRequestOutcome::Accepted,
        }
    );
    connection.close().expect("close PnL-plant connection");
    server
        .join()
        .expect("local PnL-plant TLS server did not panic")
        .expect("local PnL-plant TLS lifecycle completed");
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

/// Serves system discovery, then accepts the fresh ticker login.
fn accept_ticker_login(
    listener: &TcpListener,
    server_config: &Arc<ServerConfig>,
) -> Result<ServerWebSocket, String> {
    let (mut discovery, _) = accept_websocket(listener, server_config)?;
    assert_system_discovery_request(&read_binary(&mut discovery)?);
    discovery
        .send(Message::binary(system_info_response(&[TEST_SYSTEM], &[])))
        .map_err(|error| error.to_string())?;
    discovery.close(None).map_err(|error| error.to_string())?;
    finish_server_close(&mut discovery)?;
    drop(discovery);

    let (mut ticker, _) = accept_websocket(listener, server_config)?;
    assert_login_request(&read_binary(&mut ticker)?)?;
    ticker
        .send(Message::binary(login_response(true, &[])))
        .map_err(|error| error.to_string())?;
    Ok(ticker)
}

/// Production ticker session driver whose login targets the local fixture.
fn fixture_driver(
    endpoint: RithmicEndpoint,
    client_config: ClientConfig,
) -> (RithmicProviderDriver, RithmicProviderEvents) {
    let config = RithmicProviderConfig::try_new(
        RITHMIC_APPLICATION_NAME,
        "0.1.0",
        fixture_limits(),
        Duration::from_secs(30),
        Vec::new(),
    )
    .expect("fixture provider configuration validates");
    let callback_limits = RithmicCallbackLimits::try_new(
        NonZeroUsize::new(64).expect("nonzero events"),
        NonZeroUsize::new(64 * 1024).expect("nonzero bytes"),
        NonZeroUsize::new(32).expect("nonzero depth"),
    )
    .expect("fixture callback limits validate");
    RithmicProviderDriver::with_ticker_connector(
        config,
        callback_limits,
        Arc::new(move |credentials, application, limits, stop, abort| {
            RithmicTestSession::connect_with_abort(
                endpoint,
                credentials,
                application,
                limits,
                stop,
                abort,
                client_config.clone(),
            )
        }),
    )
}

fn fixture_credential_bytes() -> RithmicCredentialBytes {
    RithmicCredentialBytes::try_encode(FIXTURE_USER, FIXTURE_PASSWORD)
        .expect("fixture credentials encode")
}

fn session_generation(value: u64) -> SessionGeneration {
    SessionGeneration::new(NonZeroU64::new(value).expect("nonzero session generation"))
}

fn wait_for_instruments(events: &RithmicProviderEvents) {
    let deadline = Instant::now() + SERVER_IO_TIMEOUT;
    while Instant::now() < deadline {
        match events.try_recv() {
            Some(callback)
                if matches!(
                    callback.event,
                    ProviderSessionEvent::InstrumentsDiscovered { .. }
                ) =>
            {
                return;
            }
            Some(callback) => assert!(
                !matches!(callback.event, ProviderSessionEvent::Invalidated { .. }),
                "fixture session failed: {:?}",
                callback.event
            ),
            None => thread::sleep(Duration::from_millis(1)),
        }
    }
    panic!("fixture session did not reach streaming");
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

fn assert_trading_login_request(
    frame: &[u8],
    plant: rti::request_login::SysInfraType,
) -> Result<(), String> {
    let request = rti::RequestLogin::decode(frame).map_err(|error| error.to_string())?;
    if request.template_id != 10
        || request.user.as_deref() != Some(FIXTURE_USER)
        || request.password.as_deref() != Some(FIXTURE_PASSWORD)
        || request.app_name.as_deref() != Some(RITHMIC_APPLICATION_NAME)
        || request.system_name.as_deref() != Some(TEST_SYSTEM)
        || request.infra_type != Some(plant.into())
        || request.aggregated_quotes.is_some()
    {
        return Err("trading-plant login request did not match the fixture".to_string());
    }
    Ok(())
}

fn order_payload(message: RithmicOrderPlantMessage) -> DecodedOrderMessage {
    match message {
        RithmicOrderPlantMessage::Order(message) => *message,
        RithmicOrderPlantMessage::Control(control) => {
            panic!("expected an order-plant payload, got {control:?}")
        }
    }
}

fn pnl_payload(message: RithmicPnlPlantMessage) -> DecodedPnlMessage {
    match message {
        RithmicPnlPlantMessage::Pnl(message) => *message,
        RithmicPnlPlantMessage::Control(control) => {
            panic!("expected a PnL-plant payload, got {control:?}")
        }
    }
}

fn pnl_updates_response() -> Vec<u8> {
    rti::ResponsePnLPositionUpdates {
        template_id: 401,
        rp_code: vec!["0".to_string()],
        ..Default::default()
    }
    .encode_to_vec()
}

fn pnl_snapshot_response() -> Vec<u8> {
    rti::ResponsePnLPositionSnapshot {
        template_id: 403,
        rp_code: vec!["0".to_string()],
        ..Default::default()
    }
    .encode_to_vec()
}

fn account_pnl_snapshot() -> Vec<u8> {
    rti::AccountPnLPositionUpdate {
        template_id: 451,
        is_snapshot: Some(true),
        account_id: Some(FIXTURE_ACCOUNT.account_id.to_string()),
        account_balance: Some("50000.25".to_string()),
        ..Default::default()
    }
    .encode_to_vec()
}

fn order_updates_response() -> Vec<u8> {
    rti::ResponseSubscribeForOrderUpdates {
        template_id: 309,
        rp_code: vec!["0".to_string()],
        ..Default::default()
    }
    .encode_to_vec()
}

fn show_orders_response() -> Vec<u8> {
    rti::ResponseShowOrders {
        template_id: 321,
        rp_code: vec!["0".to_string()],
        ..Default::default()
    }
    .encode_to_vec()
}

fn order_notification(is_snapshot: Option<bool>) -> Vec<u8> {
    rti::RithmicOrderNotification {
        template_id: 351,
        notify_type: Some(rti::rithmic_order_notification::NotifyType::Open.into()),
        is_snapshot,
        basket_id: Some("fixture-basket".to_string()),
        account_id: Some(FIXTURE_ACCOUNT.account_id.to_string()),
        symbol: Some("ESM7".to_string()),
        exchange: Some("CME".to_string()),
        quantity: Some(1),
        price: Some(5_100.25),
        ..Default::default()
    }
    .encode_to_vec()
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
