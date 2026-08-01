//! Loopback-only insecure WebSocket endpoint validation.
//!
//! This module deliberately accepts only explicit-port `ws://` URIs whose host is a
//! loopback IP address. It rejects `wss://`, hostnames, and non-loopback addresses, so it
//! is lifecycle evidence rather than a production transport.

use core::fmt;
use std::{
    error::Error,
    net::{IpAddr, SocketAddr},
};
use tungstenite::http::Uri;

/// Validated insecure endpoint restricted to the local machine for lifecycle evidence.
///
/// This type deliberately rejects `wss://`, hostnames, and non-loopback addresses. It
/// cannot be used as a production transport or as evidence for TLS behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlainLoopbackWebSocketEndpoint {
    uri: String,
    address: SocketAddr,
}

impl PlainLoopbackWebSocketEndpoint {
    /// Validates one explicit-port `ws://` URI whose host is a loopback IP address.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed URIs, credentials, non-plain schemes, missing
    /// ports, hostnames, or non-loopback IP addresses.
    pub fn try_new(value: &str) -> Result<Self, PlainLoopbackEndpointError> {
        let uri = value
            .parse::<Uri>()
            .map_err(|_| PlainLoopbackEndpointError::InvalidUri)?;
        if uri.scheme_str() != Some("ws") {
            return Err(PlainLoopbackEndpointError::PlainWebSocketRequired);
        }
        let authority = uri
            .authority()
            .ok_or(PlainLoopbackEndpointError::MissingAuthority)?;
        if authority.as_str().contains('@') {
            return Err(PlainLoopbackEndpointError::CredentialsForbidden);
        }
        let port = uri
            .port_u16()
            .ok_or(PlainLoopbackEndpointError::ExplicitPortRequired)?;
        let host = uri.host().ok_or(PlainLoopbackEndpointError::MissingHost)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let address = host
            .parse::<IpAddr>()
            .map_err(|_| PlainLoopbackEndpointError::IpAddressRequired)?;
        if !address.is_loopback() {
            return Err(PlainLoopbackEndpointError::LoopbackRequired(address));
        }
        Ok(Self {
            uri: uri.to_string(),
            address: SocketAddr::new(address, port),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.uri
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }
}

/// Endpoint validation failures for the plain-loopback evidence connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlainLoopbackEndpointError {
    InvalidUri,
    PlainWebSocketRequired,
    MissingAuthority,
    CredentialsForbidden,
    ExplicitPortRequired,
    MissingHost,
    IpAddressRequired,
    LoopbackRequired(IpAddr),
}

impl fmt::Display for PlainLoopbackEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plain loopback WebSocket endpoint rejected: {self:?}"
        )
    }
}

impl Error for PlainLoopbackEndpointError {}
