use axiusflow_observability::{FeedConnectionState, FeedIdentity};
use axiusflow_rithmic_protocol_adapter::RithmicProviderConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicShellState {
    identity: FeedIdentity,
    connection: FeedConnectionState,
    message: String,
}

impl RithmicShellState {
    pub(crate) fn local() -> Result<Self, String> {
        let environment = RithmicProviderConfig::environment();
        let identity = FeedIdentity::try_new(
            environment.provider_id,
            environment.system_id,
            environment.environment,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            identity,
            connection: FeedConnectionState::Disconnected,
            message: "Local shell ready; provider login has not started".to_string(),
        })
    }

    pub(crate) const fn connection(&self) -> FeedConnectionState {
        self.connection
    }

    pub(crate) fn profile_label(&self) -> String {
        let provider = match self.identity.provider() {
            "rithmic" => "Rithmic",
            provider => provider,
        };
        let system = match self.identity.system() {
            "RITHMIC_TEST" => "Rithmic Test",
            system => system,
        };
        format!("{provider} · {system} · {}", self.identity.environment())
    }
}

pub(crate) const fn connection_label(connection: FeedConnectionState) -> &'static str {
    match connection {
        FeedConnectionState::Disconnected => "Not connected",
        FeedConnectionState::Discovering => "Discovering Test systems",
        FeedConnectionState::Authenticating => "Authenticating",
        FeedConnectionState::Streaming => "Live market data",
        FeedConnectionState::Recovering => "Reconnecting",
        FeedConnectionState::Stopped => "Stopped",
    }
}

impl RithmicShellState {
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_shell_is_available_before_login_or_history() {
        let shell = RithmicShellState::local().expect("fixed Rithmic Test profile validates");
        assert_eq!(shell.identity.provider(), "rithmic");
        assert_eq!(shell.identity.system(), "RITHMIC_TEST");
        assert_eq!(shell.identity.environment(), "Test");
        assert_eq!(shell.connection(), FeedConnectionState::Disconnected);
        assert_eq!(connection_label(shell.connection()), "Not connected");
        assert_eq!(shell.profile_label(), "Rithmic · Rithmic Test · Test");
        assert!(shell.message().contains("login has not started"));
    }
}
