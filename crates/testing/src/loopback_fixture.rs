//! Shared loopback transport fixtures used by every WebSocket scenario module.
//!
//! This module is a dependency leaf: it owns the loopback server accept and stream
//! configuration helpers, the bounded WebSocket session/config builders, and the
//! plain-loopback owner constructors. Keeping them here prevents the scenario modules
//! from depending on each other in a cycle.

use crate::binary_fixture::{BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture};
use crate::harness_error::{ConformanceHarnessError, websocket_error};
use axiusflow_application::ReplayProvenance;
use axiusflow_stream_websocket_adapter::{
    MarketWebSocketConfig, MarketWebSocketSession, PlainLoopbackLifecycleConfig,
    PlainLoopbackMarketWebSocketOwner, PlainLoopbackWebSocketEndpoint,
};
use std::{net::TcpListener, num::NonZeroUsize, thread, time::Duration};

pub(crate) fn accept_runtime_connection(
    listener: &TcpListener,
    timeout: Duration,
) -> Result<std::net::TcpStream, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("runtime chart server accept timed out".to_string());
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub(crate) fn configure_runtime_server_stream(stream: &std::net::TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())
}

pub(crate) fn websocket_fixture_config(
    fixture: &BinaryMarketStreamFixture,
    maximum_publications: usize,
) -> Result<MarketWebSocketConfig, ConformanceHarnessError> {
    MarketWebSocketConfig::try_new(
        fixture.maximum_buffered_bytes,
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
        NonZeroUsize::new(maximum_publications).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(websocket_error)
}

pub(crate) fn websocket_fixture_session(
    fixture: &BinaryMarketStreamFixture,
    config: MarketWebSocketConfig,
) -> Result<MarketWebSocketSession, ConformanceHarnessError> {
    MarketWebSocketSession::try_new(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.convention.clone(),
        ReplayProvenance::EmbeddedFixture,
        config,
    )
    .map_err(websocket_error)
}

pub(crate) fn plain_loopback_owner(
    fixture: &BinaryMarketStreamFixture,
    endpoint: PlainLoopbackWebSocketEndpoint,
    event_capacity: usize,
    connection_attempt_limit: usize,
) -> Result<PlainLoopbackMarketWebSocketOwner, ConformanceHarnessError> {
    plain_loopback_owner_with_timeout(
        fixture,
        endpoint,
        event_capacity,
        8,
        connection_attempt_limit,
        Duration::from_secs(3),
    )
}

pub(crate) fn plain_loopback_owner_with_timeout(
    fixture: &BinaryMarketStreamFixture,
    endpoint: PlainLoopbackWebSocketEndpoint,
    event_capacity: usize,
    publication_capacity: usize,
    connection_attempt_limit: usize,
    io_timeout: Duration,
) -> Result<PlainLoopbackMarketWebSocketOwner, ConformanceHarnessError> {
    let session = websocket_fixture_session(
        fixture,
        websocket_fixture_config(fixture, publication_capacity)?,
    )?;
    let config = PlainLoopbackLifecycleConfig::try_new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(event_capacity).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(connection_attempt_limit).unwrap_or(NonZeroUsize::MIN),
        io_timeout,
    )
    .map_err(websocket_error)?;
    PlainLoopbackMarketWebSocketOwner::try_new(endpoint, config, session).map_err(websocket_error)
}
