//! Authenticode publisher verification for Windows executables.
//!
//! `WinVerifyTrust` verifies the embedded signature and its certificate
//! chain; this module then requires the expected signer certificate and a
//! timestamp countersignature. Revocation is not checked and URL retrieval is
//! cache-only, so an audit never waits on the network.

use std::{error::Error, fmt, io, path::Path};

use crate::windows::trust::{Verification, verify_embedded_signature};

/// Why an executable failed publisher verification.
#[derive(Debug)]
pub enum TrustError {
    /// `WinVerifyTrust` refused the embedded signature with this status.
    Untrusted(i32),
    /// The trusted signature exposes no readable signer certificate.
    SignerUnavailable,
    /// The signer certificate is not the expected publisher.
    SignerMismatch,
    /// The signature carries no timestamp countersignature.
    MissingTimestamp,
    /// The verification request could not be built.
    Request(io::Error),
}

impl fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untrusted(status) => {
                write!(
                    formatter,
                    "Windows does not trust the signature ({status:#010x})"
                )
            }
            Self::SignerUnavailable => formatter.write_str("the signer certificate is unavailable"),
            Self::SignerMismatch => formatter.write_str("the signer is not the expected publisher"),
            Self::MissingTimestamp => formatter.write_str("the signature is not timestamped"),
            Self::Request(error) => write!(formatter, "verification could not start: {error}"),
        }
    }
}

impl Error for TrustError {}

/// Verifies that `path` carries a trusted, timestamped embedded Authenticode
/// signature whose signer certificate has the SHA-1 thumbprint
/// `expected_signer`.
///
/// # Errors
/// Returns the first failed requirement.
pub fn verify_publisher(path: &Path, expected_signer: &[u8; 20]) -> Result<(), TrustError> {
    match verify_embedded_signature(path).map_err(TrustError::Request)? {
        Verification::Untrusted(status) => Err(TrustError::Untrusted(status)),
        Verification::SignerUnavailable => Err(TrustError::SignerUnavailable),
        Verification::Trusted(signature) if signature.signer_thumbprint != *expected_signer => {
            Err(TrustError::SignerMismatch)
        }
        Verification::Trusted(signature) if !signature.timestamped => {
            Err(TrustError::MissingTimestamp)
        }
        Verification::Trusted(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use super::{TrustError, verify_publisher};
    use crate::windows::trust::{Verification, verify_embedded_signature};

    const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100_u32.cast_signed();

    /// An executable with an embedded, timestamped Microsoft signature.
    /// Edge ships on every supported Windows and hosted runner; Defender's
    /// signature stub and the removal tool are fallbacks.
    fn signed_fixture() -> PathBuf {
        let program_files = ["ProgramFiles(x86)", "ProgramFiles"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(|root| PathBuf::from(root).join("Microsoft/Edge/Application/msedge.exe"));
        let system = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .into_iter()
            .flat_map(|root| {
                [
                    root.join("System32/MpSigStub.exe"),
                    root.join("System32/MRT.exe"),
                ]
            });
        program_files
            .chain(system)
            .find(|candidate| candidate.is_file())
            .expect("an embedded-signed Microsoft executable is installed")
    }

    /// The signer thumbprint Windows PowerShell reports, as an independent
    /// reading of the same signature. A PowerShell 7 parent's module path
    /// would hide the Windows PowerShell security module, so it is cleared.
    fn powershell_thumbprint(path: &Path) -> [u8; 20] {
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-AuthenticodeSignature -LiteralPath $env:AERIS_TRUST_FIXTURE).SignerCertificate.Thumbprint",
            ])
            .env("AERIS_TRUST_FIXTURE", path)
            .env_remove("PSModulePath")
            .output()
            .expect("PowerShell runs");
        let hex = String::from_utf8(output.stdout).expect("thumbprint text");
        let hex = hex.trim();
        assert_eq!(
            hex.len(),
            40,
            "thumbprint {hex:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut thumbprint = [0_u8; 20];
        for (index, byte) in thumbprint.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex");
        }
        thumbprint
    }

    #[test]
    fn a_signed_executable_verifies_against_its_publisher() {
        let fixture = signed_fixture();
        let Verification::Trusted(signature) =
            verify_embedded_signature(&fixture).expect("verification runs")
        else {
            panic!("{} is not trusted", fixture.display());
        };
        assert!(signature.timestamped);
        assert_eq!(signature.signer_thumbprint, powershell_thumbprint(&fixture));
        verify_publisher(&fixture, &signature.signer_thumbprint).expect("trusted publisher");
    }

    #[test]
    fn a_different_signer_is_rejected() {
        let fixture = signed_fixture();
        let mut other = powershell_thumbprint(&fixture);
        other[0] ^= 0xff;
        assert!(matches!(
            verify_publisher(&fixture, &other),
            Err(TrustError::SignerMismatch)
        ));
    }

    #[test]
    fn an_unsigned_executable_is_untrusted() {
        let unsigned = std::env::current_exe().expect("test executable");
        assert!(matches!(
            verify_publisher(&unsigned, &[0; 20]),
            Err(TrustError::Untrusted(TRUST_E_NOSIGNATURE))
        ));
    }

    #[test]
    fn a_catalog_only_system_file_has_no_embedded_signature() {
        let catalog_signed = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32/cmd.exe");
        assert!(matches!(
            verify_publisher(&catalog_signed, &[0; 20]),
            Err(TrustError::Untrusted(TRUST_E_NOSIGNATURE))
        ));
    }

    #[test]
    fn a_modified_signed_executable_is_untrusted() {
        let fixture = signed_fixture();
        let expected = powershell_thumbprint(&fixture);
        let directory =
            std::env::temp_dir().join(format!("aeris-platform-sys-trust-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("fixture directory");
        let copy = directory.join("modified.exe");
        let mut bytes = fs::read(&fixture).expect("read fixture");
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::write(&copy, bytes).expect("write modified copy");
        let result = verify_publisher(&copy, &expected);
        fs::remove_dir_all(&directory).expect("remove fixture directory");
        assert!(
            matches!(result, Err(TrustError::Untrusted(_))),
            "{result:?}"
        );
    }
}
