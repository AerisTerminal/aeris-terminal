use aeris_observability::{FeedConnectionState, FeedIdentity};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicShellState {
    pub(crate) identity: FeedIdentity,
    connection: FeedConnectionState,
}

impl RithmicShellState {
    pub(crate) fn local() -> Result<Self, String> {
        let identity = FeedIdentity::try_new("rithmic", "RITHMIC_TEST", "Test")
            .map_err(|error| error.to_string())?;
        Ok(Self {
            identity,
            connection: FeedConnectionState::Discovering,
        })
    }

    pub(crate) const fn connection(&self) -> FeedConnectionState {
        self.connection
    }

    /// The server and system come from the saved login, which the runtime alone reads, so the
    /// pre-login shell names only the provider.
    pub(crate) fn profile_label(&self) -> String {
        match self.identity.provider() {
            "rithmic" => "Rithmic".to_string(),
            provider => provider.to_string(),
        }
    }
}

#[cfg(test)]
pub(crate) const fn connection_label(connection: FeedConnectionState) -> &'static str {
    match connection {
        FeedConnectionState::Disconnected => "Not connected",
        FeedConnectionState::Discovering => "Discovering Rithmic systems",
        FeedConnectionState::Authenticating => "Authenticating",
        FeedConnectionState::Streaming => "Live market data",
        FeedConnectionState::Recovering => "Reconnecting",
        FeedConnectionState::Stopped => "Stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_shell_is_available_before_login_or_history() {
        let shell = RithmicShellState::local().expect("fixed Rithmic profile validates");
        assert_eq!(shell.identity.provider(), "rithmic");
        assert_eq!(shell.identity.system(), "RITHMIC_TEST");
        assert_eq!(shell.identity.environment(), "Test");
        assert_eq!(shell.connection(), FeedConnectionState::Discovering);
        assert_eq!(
            connection_label(shell.connection()),
            "Discovering Rithmic systems"
        );
        assert_eq!(shell.profile_label(), "Rithmic");
    }
}
