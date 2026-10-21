use std::{fmt, time::Duration};

const RITHMIC_PORT: u16 = 443;

/// One R | Protocol connect point. Rithmic issued the production points with
/// Aeris's completed conformance; every point serves every Rithmic system the
/// login is permissioned for, and system discovery reports which ones.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RithmicGateway {
    Chicago,
    NewYork,
    Colo75,
    SaoPaulo,
    Frankfurt,
    Ireland,
    CapeTown,
    Mumbai,
    Singapore,
    HongKong,
    Seoul,
    Tokyo,
    Sydney,
    /// The conformance environment; it serves only the Rithmic Test system.
    Test,
}

impl RithmicGateway {
    /// Every connect point, the core Chicago point first.
    pub const ALL: [Self; 14] = [
        Self::Chicago,
        Self::NewYork,
        Self::Colo75,
        Self::SaoPaulo,
        Self::Frankfurt,
        Self::Ireland,
        Self::CapeTown,
        Self::Mumbai,
        Self::Singapore,
        Self::HongKong,
        Self::Seoul,
        Self::Tokyo,
        Self::Sydney,
        Self::Test,
    ];

    /// The name shown when choosing a server.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chicago => "Chicago (core)",
            Self::NewYork => "New York",
            Self::Colo75 => "Colo75",
            Self::SaoPaulo => "São Paulo",
            Self::Frankfurt => "Frankfurt",
            Self::Ireland => "Ireland",
            Self::CapeTown => "Cape Town",
            Self::Mumbai => "Mumbai",
            Self::Singapore => "Singapore",
            Self::HongKong => "Hong Kong",
            Self::Seoul => "Seoul",
            Self::Tokyo => "Tokyo",
            Self::Sydney => "Sydney",
            Self::Test => "Rithmic Test",
        }
    }

    #[must_use]
    pub const fn host(self) -> &'static str {
        match self {
            Self::Chicago => "rprotocol.rithmic.com",
            Self::NewYork => "rprotocol-nyc.rithmic.com",
            Self::Colo75 => "rprotocol-colo75.rithmic.com",
            Self::SaoPaulo => "rprotocol-br.rithmic.com",
            Self::Frankfurt => "rprotocol-de.rithmic.com",
            Self::Ireland => "rprotocol-ie.rithmic.com",
            Self::CapeTown => "rprotocol-za.rithmic.com",
            Self::Mumbai => "rprotocol-in.rithmic.com",
            Self::Singapore => "rprotocol-sg.rithmic.com",
            Self::HongKong => "rprotocol-hk.rithmic.com",
            Self::Seoul => "rprotocol-kr.rithmic.com",
            Self::Tokyo => "rprotocol-jp.rithmic.com",
            Self::Sydney => "rprotocol-au.rithmic.com",
            Self::Test => "rituz00100.rithmic.com",
        }
    }

    const fn url(self) -> &'static str {
        match self {
            Self::Chicago => "wss://rprotocol.rithmic.com:443",
            Self::NewYork => "wss://rprotocol-nyc.rithmic.com:443",
            Self::Colo75 => "wss://rprotocol-colo75.rithmic.com:443",
            Self::SaoPaulo => "wss://rprotocol-br.rithmic.com:443",
            Self::Frankfurt => "wss://rprotocol-de.rithmic.com:443",
            Self::Ireland => "wss://rprotocol-ie.rithmic.com:443",
            Self::CapeTown => "wss://rprotocol-za.rithmic.com:443",
            Self::Mumbai => "wss://rprotocol-in.rithmic.com:443",
            Self::Singapore => "wss://rprotocol-sg.rithmic.com:443",
            Self::HongKong => "wss://rprotocol-hk.rithmic.com:443",
            Self::Seoul => "wss://rprotocol-kr.rithmic.com:443",
            Self::Tokyo => "wss://rprotocol-jp.rithmic.com:443",
            Self::Sydney => "wss://rprotocol-au.rithmic.com:443",
            Self::Test => "wss://rituz00100.rithmic.com:443",
        }
    }

    /// Stable one-byte code stored in the saved login; never renumber a code.
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Test => 0,
            Self::Chicago => 1,
            Self::NewYork => 2,
            Self::Sydney => 3,
            Self::SaoPaulo => 4,
            Self::Colo75 => 5,
            Self::Frankfurt => 6,
            Self::HongKong => 7,
            Self::Ireland => 8,
            Self::Mumbai => 9,
            Self::Seoul => 10,
            Self::CapeTown => 11,
            Self::Tokyo => 12,
            Self::Singapore => 13,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|gateway| gateway.code() == code)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicEndpoint {
    pub(crate) url: &'static str,
    pub(crate) host: &'static str,
    pub(crate) port: u16,
}

impl RithmicEndpoint {
    pub(crate) const fn for_gateway(gateway: RithmicGateway) -> Self {
        Self {
            url: gateway.url(),
            host: gateway.host(),
            port: RITHMIC_PORT,
        }
    }

    pub(crate) fn validate(self) -> Result<Self, RithmicSessionError> {
        let uri = self
            .url
            .parse::<tungstenite::http::Uri>()
            .map_err(|_| RithmicSessionError::InvalidEndpoint)?;
        let expected_authority = format!("{}:{}", self.host, self.port);
        if uri.scheme_str() != Some("wss")
            || uri
                .authority()
                .map(tungstenite::http::uri::Authority::as_str)
                != Some(expected_authority.as_str())
            || uri.host() != Some(self.host)
            || uri.port_u16() != Some(self.port)
            || uri
                .path_and_query()
                .map(tungstenite::http::uri::PathAndQuery::as_str)
                != Some("/")
        {
            return Err(RithmicSessionError::InvalidEndpoint);
        }
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) const fn loopback(url: &'static str, port: u16) -> Self {
        Self {
            url,
            host: "localhost",
            port,
        }
    }
}

/// Resource and deadline limits for one Rithmic connection attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicSessionLimits {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub response_timeout: Duration,
    pub close_timeout: Duration,
    pub maximum_message_bytes: usize,
    pub maximum_write_buffer_bytes: usize,
}

impl Default for RithmicSessionLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            handshake_timeout: Duration::from_secs(15),
            response_timeout: Duration::from_secs(15),
            close_timeout: Duration::from_secs(5),
            maximum_message_bytes: 1024 * 1024,
            maximum_write_buffer_bytes: 2 * 1024 * 1024,
        }
    }
}

impl RithmicSessionLimits {
    pub(crate) fn validate(self) -> Result<Self, RithmicSessionError> {
        const MAXIMUM_TIMEOUT: Duration = Duration::from_mins(1);
        if self.connect_timeout.is_zero()
            || self.handshake_timeout.is_zero()
            || self.response_timeout.is_zero()
            || self.close_timeout.is_zero()
            || self.connect_timeout > MAXIMUM_TIMEOUT
            || self.handshake_timeout > MAXIMUM_TIMEOUT
            || self.response_timeout > MAXIMUM_TIMEOUT
            || self.close_timeout > MAXIMUM_TIMEOUT
            || self.maximum_message_bytes == 0
            || self.maximum_message_bytes > 1024 * 1024
            || self.maximum_write_buffer_bytes < self.maximum_message_bytes
            || self.maximum_write_buffer_bytes > 4 * 1024 * 1024
        {
            return Err(RithmicSessionError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Whether reconnecting can change the outcome of a failed session attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryDisposition {
    Transient,
    Terminal,
}

/// Redacted failure classes for the Rithmic session boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicSessionError {
    KitUnavailable,
    InvalidEndpoint,
    InvalidLimits,
    Cancelled,
    Resolve,
    Connect,
    Tls,
    Handshake,
    Deadline,
    Transport,
    RequestInFlight,
    /// The request is invalid for this plant or for the session's current
    /// ordering state, for example a snapshot before its update subscription.
    RequestNotPermitted,
    /// The provider refused a request that the session depends on.
    RequestRejected,
    UnexpectedMessage,
    Protocol,
    /// The server does not offer the system the login names.
    SystemUnavailable,
    DiscoveryRejected,
    DiscoveryClose,
    /// Rithmic refused the login. Its answer does not say whether the user ID
    /// and password were wrong or the account lacks access for Aeris: it
    /// returns `rp_code` 13, "permission denied", even for an unknown user.
    LoginRejected,
    SchemaMismatch,
}

impl RithmicSessionError {
    #[must_use]
    pub const fn retry_disposition(self) -> RetryDisposition {
        match self {
            Self::Cancelled
            | Self::KitUnavailable
            | Self::InvalidEndpoint
            | Self::InvalidLimits
            | Self::Tls
            | Self::Handshake
            | Self::RequestInFlight
            | Self::RequestNotPermitted
            | Self::RequestRejected
            | Self::UnexpectedMessage
            | Self::Protocol
            | Self::SystemUnavailable
            | Self::DiscoveryRejected
            | Self::LoginRejected
            | Self::SchemaMismatch => RetryDisposition::Terminal,
            Self::Resolve
            | Self::Connect
            | Self::Deadline
            | Self::Transport
            | Self::DiscoveryClose => RetryDisposition::Transient,
        }
    }
}

impl fmt::Display for RithmicSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::KitUnavailable => "Rithmic protocol kit is unavailable",
            Self::InvalidEndpoint => "Rithmic server address is invalid",
            Self::InvalidLimits => "Rithmic session limits are invalid",
            Self::Cancelled => "Rithmic session was cancelled",
            Self::Resolve => "Rithmic endpoint resolution failed",
            Self::Connect => "Rithmic endpoint connection failed",
            Self::Tls => "Rithmic TLS setup failed",
            Self::Handshake => "Rithmic WebSocket handshake failed",
            Self::Deadline => "Rithmic session deadline expired",
            Self::Transport => "Rithmic transport failed",
            Self::RequestInFlight => "a Rithmic request of this kind is already in flight",
            Self::RequestNotPermitted => {
                "the Rithmic request is not permitted in this session state"
            }
            Self::RequestRejected => "Rithmic rejected a required session request",
            Self::UnexpectedMessage => "Rithmic returned an unexpected message",
            Self::Protocol => "Rithmic protocol validation failed",
            Self::SystemUnavailable => "this Rithmic server does not offer the selected system",
            Self::DiscoveryRejected => "Rithmic system discovery was rejected",
            Self::DiscoveryClose => "Rithmic discovery connection did not close cleanly",
            Self::LoginRejected => {
                "Rithmic refused the login; check the user ID, password and system, and that \
                 the account has market data and Aeris enabled"
            }
            Self::SchemaMismatch => "Rithmic protocol schema does not match the installed kit",
        })
    }
}

impl std::error::Error for RithmicSessionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gateway_is_a_valid_wss_endpoint_with_a_unique_stable_code() {
        let mut codes = std::collections::BTreeSet::new();
        let mut hosts = std::collections::BTreeSet::new();
        for gateway in RithmicGateway::ALL {
            let endpoint = RithmicEndpoint::for_gateway(gateway);
            assert_eq!(endpoint.validate(), Ok(endpoint), "{gateway:?}");
            assert_eq!(endpoint.port, 443);
            assert!(codes.insert(gateway.code()), "{gateway:?} code repeats");
            assert!(hosts.insert(gateway.host()), "{gateway:?} host repeats");
            assert_eq!(RithmicGateway::from_code(gateway.code()), Some(gateway));
        }
        assert_eq!(RithmicGateway::ALL[0], RithmicGateway::Chicago);
        assert_eq!(RithmicGateway::Chicago.host(), "rprotocol.rithmic.com");
        assert_eq!(RithmicGateway::Test.host(), "rituz00100.rithmic.com");
        assert_eq!(RithmicGateway::Test.code(), 0);
        assert_eq!(RithmicGateway::Chicago.code(), 1);
        assert_eq!(RithmicGateway::from_code(14), None);
    }

    #[test]
    fn loopback_endpoint_and_limits_are_bounded() {
        let loopback = RithmicEndpoint::loopback("wss://localhost:1234", 1234);
        assert_eq!(loopback.host, "localhost");
        assert!(RithmicSessionLimits::default().validate().is_ok());
        assert!(matches!(
            RithmicSessionLimits {
                maximum_message_bytes: 0,
                ..RithmicSessionLimits::default()
            }
            .validate(),
            Err(RithmicSessionError::InvalidLimits)
        ));
    }

    #[test]
    fn failure_output_is_coarse_and_retry_classification_is_explicit() {
        assert_eq!(
            RithmicSessionError::LoginRejected.retry_disposition(),
            RetryDisposition::Terminal
        );
        assert_eq!(
            RithmicSessionError::Connect.retry_disposition(),
            RetryDisposition::Transient
        );
        let output = format!("{:?}", RithmicSessionError::Protocol);
        assert!(!output.contains("password"));
        assert!(!output.contains("provider"));
    }
}
