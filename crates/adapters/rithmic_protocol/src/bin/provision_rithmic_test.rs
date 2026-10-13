use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use aeris_rithmic_protocol_adapter::{
    RITHMIC_LOGIN_VAULT_KEY, RITHMIC_VAULT_SERVICE, RithmicCredentialBytes, RithmicCredentials,
    RithmicGateway,
};
use rpassword::prompt_password;
use std::{fmt, io::IsTerminal};
use zeroize::Zeroizing;

/// The only system the conformance gateway serves; production logins are
/// saved from the desktop's Rithmic connect dialog instead.
const RITHMIC_TEST_SYSTEM: &str = "Rithmic Test";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProvisionError {
    TerminalRequired,
    PromptUnavailable,
    InvalidCredentials,
    VaultUnavailable,
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TerminalRequired => "credential provisioning requires an interactive terminal",
            Self::PromptUnavailable => "the protected credential prompt is unavailable",
            Self::InvalidCredentials => "the entered Rithmic credentials are invalid",
            Self::VaultUnavailable => "the native credential vault is unavailable",
        })
    }
}

fn main() {
    if let Err(error) = provision() {
        eprintln!("Rithmic Test credential provisioning failed: {error}");
        std::process::exit(1);
    }
    println!("Rithmic Test credentials stored in the native credential vault.");
}

fn provision() -> Result<(), ProvisionError> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(ProvisionError::TerminalRequired);
    }
    let user = Zeroizing::new(
        prompt_password("Rithmic Test user: ").map_err(|_| ProvisionError::PromptUnavailable)?,
    );
    let password = Zeroizing::new(
        prompt_password("Rithmic Test password: ")
            .map_err(|_| ProvisionError::PromptUnavailable)?,
    );
    let encoded = RithmicCredentialBytes::try_encode(RithmicCredentials {
        gateway: RithmicGateway::Test,
        system_name: RITHMIC_TEST_SYSTEM,
        user: user.as_str(),
        password: password.as_str(),
    })
    .map_err(|_| ProvisionError::InvalidCredentials)?;
    let vault = NativeCredentialVault::new(RITHMIC_VAULT_SERVICE)
        .map_err(|_| ProvisionError::VaultUnavailable)?;
    vault
        .store(RITHMIC_LOGIN_VAULT_KEY, encoded.as_bytes())
        .map_err(|_| ProvisionError::VaultUnavailable)
}
