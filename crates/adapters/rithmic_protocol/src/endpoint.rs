use std::{fmt, time::Duration};

pub(crate) const RITHMIC_TEST_HOST: &str = "rituz00100.rithmic.com";
pub(crate) const RITHMIC_TEST_PORT: u16 = 443;
pub(crate) const RITHMIC_TEST_URL: &str = "wss://rituz00100.rithmic.com:443";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicEndpoint {
    pub(crate) url: &'static str,
    pub(crate) host: &'static str,
    pub(crate) port: u16,
}

impl RithmicEndpoint {
    pub(crate) const TEST: Self = Self {
        url: RITHMIC_TEST_URL,
        host: RITHMIC_TEST_HOST,
        port: RITHMIC_TEST_PORT,
    };

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

/// Redacted failure classes for the Rithmic Test session boundary.
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
    TestSystemUnavailable,
    DiscoveryRejected,
    DiscoveryClose,
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
            | Self::TestSystemUnavailable
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
            Self::InvalidEndpoint => "Rithmic Test endpoint is invalid",
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
            Self::TestSystemUnavailable => "Rithmic Test is unavailable",
            Self::DiscoveryRejected => "Rithmic system discovery was rejected",
            Self::DiscoveryClose => "Rithmic discovery connection did not close cleanly",
            Self::LoginRejected => "Rithmic Test login was rejected",
            Self::SchemaMismatch => "Rithmic protocol schema does not match the installed kit",
        })
    }
}

impl std::error::Error for RithmicSessionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_endpoint_and_limits_are_fixed_and_bounded() {
        assert_eq!(RithmicEndpoint::TEST.url, RITHMIC_TEST_URL);
        assert_eq!(RithmicEndpoint::TEST.host, RITHMIC_TEST_HOST);
        assert_eq!(RithmicEndpoint::TEST.port, 443);
        assert_eq!(RithmicEndpoint::TEST.validate(), Ok(RithmicEndpoint::TEST));
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
