use axiusflow_observability::{FeedConnectionState, FeedIdentity};
use axiusflow_rithmic_protocol_adapter::RithmicProviderConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicShellState {
    pub identity: FeedIdentity,
    connection: FeedConnectionState,
    message: String,
}

impl RithmicShellState {
    /// Creates the local shell state from provider configuration.
    ///
    /// # Errors
    /// Returns an error when the configured feed identity is invalid.
    pub fn local() -> Result<Self, String> {
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

    #[must_use]
    pub const fn connection(&self) -> FeedConnectionState {
        self.connection
    }

    #[must_use]
    pub fn profile_label(&self) -> String {
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

impl RithmicShellState {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
