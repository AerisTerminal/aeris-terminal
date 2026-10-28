use crate::{RithmicCredentials, RithmicGateway};
use core::fmt;
use zeroize::Zeroize;

const CREDENTIAL_VERSION: u8 = 2;
/// Version, gateway code, then big-endian system, user and password lengths.
const HEADER_BYTES: usize = 8;

/// Maximum UTF-8 bytes accepted in either Rithmic credential field.
pub const MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES: usize = 256;

/// Maximum UTF-8 bytes accepted in a Rithmic system name.
pub const MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES: usize = 128;

/// Maximum bytes accepted in one versioned Rithmic credential blob.
pub const MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES: usize =
    HEADER_BYTES + MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES + (2 * MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES);
const _: () =
    assert!(MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES <= aeris_platform_runtime::MAX_SECRET_BYTES);

/// Redacted validation failures for a versioned Rithmic credential blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicCredentialError {
    BlobTooLarge { maximum: usize },
    Truncated,
    UnsupportedVersion,
    UnknownGateway,
    LengthMismatch,
    SystemEmpty,
    UserEmpty,
    PasswordEmpty,
    SystemTooLong { maximum: usize },
    UserTooLong { maximum: usize },
    PasswordTooLong { maximum: usize },
    SystemInvalidUtf8,
    UserInvalidUtf8,
    PasswordInvalidUtf8,
    SystemControlCharacter,
    UserControlCharacter,
    PasswordControlCharacter,
}

impl fmt::Display for RithmicCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic credential blob is invalid: {self:?}")
    }
}

impl std::error::Error for RithmicCredentialError {}

/// One saved Rithmic login, the server and system with the user's credentials,
/// as owned bytes that zeroize every attempt-local copy on drop.
pub struct RithmicCredentialBytes(Vec<u8>);

impl RithmicCredentialBytes {
    /// Encodes a validated login into the version-2 vault representation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for empty, oversized, or control-containing fields.
    pub fn try_encode(credentials: RithmicCredentials<'_>) -> Result<Self, RithmicCredentialError> {
        validate_fields(credentials)?;
        let RithmicCredentials {
            gateway,
            system_name,
            user,
            password,
        } = credentials;
        let mut bytes =
            Vec::with_capacity(HEADER_BYTES + system_name.len() + user.len() + password.len());
        bytes.push(CREDENTIAL_VERSION);
        bytes.push(gateway.code());
        for field in [system_name, user, password] {
            let length =
                u16::try_from(field.len()).map_err(|_| RithmicCredentialError::LengthMismatch)?;
            bytes.extend_from_slice(&length.to_be_bytes());
        }
        bytes.extend_from_slice(system_name.as_bytes());
        bytes.extend_from_slice(user.as_bytes());
        bytes.extend_from_slice(password.as_bytes());
        Ok(Self(bytes))
    }

    /// Copies and validates opaque bytes loaded from the credential vault.
    ///
    /// The copy is suitable for transfer to one connection-attempt thread and
    /// is zeroized when that attempt releases it.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for malformed, unbounded, or invalid credentials.
    pub fn try_copy_from_vault(bytes: &[u8]) -> Result<Self, RithmicCredentialError> {
        if bytes.len() > MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES {
            return Err(RithmicCredentialError::BlobTooLarge {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES,
            });
        }
        let owned = Self(bytes.to_vec());
        owned.credentials()?;
        Ok(owned)
    }

    /// Exposes the encoded bytes for storage in an opaque credential vault.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the borrowed login without allocating additional strings.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the owned bytes are malformed.
    pub fn credentials(&self) -> Result<RithmicCredentials<'_>, RithmicCredentialError> {
        parse_credentials(&self.0)
    }
}

impl Clone for RithmicCredentialBytes {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for RithmicCredentialBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicCredentialBytes")
            .field("encoded_bytes", &self.0.len())
            .finish()
    }
}

impl Drop for RithmicCredentialBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn parse_credentials(bytes: &[u8]) -> Result<RithmicCredentials<'_>, RithmicCredentialError> {
    if bytes.len() > MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES {
        return Err(RithmicCredentialError::BlobTooLarge {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES,
        });
    }
    match bytes.first() {
        None => return Err(RithmicCredentialError::Truncated),
        Some(&version) if version != CREDENTIAL_VERSION => {
            return Err(RithmicCredentialError::UnsupportedVersion);
        }
        Some(_) => {}
    }
    let header = bytes
        .get(..HEADER_BYTES)
        .ok_or(RithmicCredentialError::Truncated)?;
    let gateway =
        RithmicGateway::from_code(header[1]).ok_or(RithmicCredentialError::UnknownGateway)?;
    let system_length = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let user_length = usize::from(u16::from_be_bytes([header[4], header[5]]));
    let password_length = usize::from(u16::from_be_bytes([header[6], header[7]]));
    validate_lengths(system_length, user_length, password_length)?;
    if bytes.len() != HEADER_BYTES + system_length + user_length + password_length {
        return Err(RithmicCredentialError::LengthMismatch);
    }
    let system_end = HEADER_BYTES + system_length;
    let user_end = system_end + user_length;
    let credentials = RithmicCredentials {
        gateway,
        system_name: std::str::from_utf8(&bytes[HEADER_BYTES..system_end])
            .map_err(|_| RithmicCredentialError::SystemInvalidUtf8)?,
        user: std::str::from_utf8(&bytes[system_end..user_end])
            .map_err(|_| RithmicCredentialError::UserInvalidUtf8)?,
        password: std::str::from_utf8(&bytes[user_end..])
            .map_err(|_| RithmicCredentialError::PasswordInvalidUtf8)?,
    };
    validate_fields(credentials)?;
    Ok(credentials)
}

fn validate_lengths(
    system: usize,
    user: usize,
    password: usize,
) -> Result<(), RithmicCredentialError> {
    if system > MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES {
        return Err(RithmicCredentialError::SystemTooLong {
            maximum: MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES,
        });
    }
    if user > MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES {
        return Err(RithmicCredentialError::UserTooLong {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
        });
    }
    if password > MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES {
        return Err(RithmicCredentialError::PasswordTooLong {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
        });
    }
    Ok(())
}

fn validate_fields(credentials: RithmicCredentials<'_>) -> Result<(), RithmicCredentialError> {
    let RithmicCredentials {
        system_name,
        user,
        password,
        ..
    } = credentials;
    if system_name.is_empty() {
        return Err(RithmicCredentialError::SystemEmpty);
    }
    if user.is_empty() {
        return Err(RithmicCredentialError::UserEmpty);
    }
    if password.is_empty() {
        return Err(RithmicCredentialError::PasswordEmpty);
    }
    validate_lengths(system_name.len(), user.len(), password.len())?;
    if system_name.chars().any(char::is_control) {
        return Err(RithmicCredentialError::SystemControlCharacter);
    }
    if user.chars().any(char::is_control) {
        return Err(RithmicCredentialError::UserControlCharacter);
    }
    if password.chars().any(char::is_control) {
        return Err(RithmicCredentialError::PasswordControlCharacter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM: &str = "Rithmic Paper Trading";
    const USER: &str = "credential-fixture-user";
    const PASSWORD: &str = "credential-fixture-password";

    const fn login<'a>(
        system_name: &'a str,
        user: &'a str,
        password: &'a str,
    ) -> RithmicCredentials<'a> {
        RithmicCredentials {
            gateway: RithmicGateway::Chicago,
            system_name,
            user,
            password,
        }
    }

    fn encode(
        system_name: &str,
        user: &str,
        password: &str,
    ) -> Result<RithmicCredentialBytes, RithmicCredentialError> {
        RithmicCredentialBytes::try_encode(login(system_name, user, password))
    }

    fn credential_error(
        result: Result<RithmicCredentialBytes, RithmicCredentialError>,
    ) -> RithmicCredentialError {
        result.expect_err("invalid credential blob fails closed")
    }

    fn length(field: &str) -> [u8; 2] {
        u16::try_from(field.len())
            .expect("fixture field length fits")
            .to_be_bytes()
    }

    #[test]
    fn version_two_round_trip_is_big_endian_and_borrowed() {
        let encoded = encode(SYSTEM, USER, PASSWORD).expect("bounded login encodes");
        let bytes = encoded.as_bytes();
        assert_eq!(bytes[0], 2);
        assert_eq!(bytes[1], RithmicGateway::Chicago.code());
        assert_eq!(&bytes[2..4], &length(SYSTEM));
        assert_eq!(&bytes[4..6], &length(USER));
        assert_eq!(&bytes[6..8], &length(PASSWORD));

        let copied = RithmicCredentialBytes::try_copy_from_vault(bytes)
            .expect("vault bytes validate and copy");
        let credentials = copied.credentials().expect("copied login parses");
        assert_eq!(credentials.gateway, RithmicGateway::Chicago);
        assert_eq!(credentials.system_name, SYSTEM);
        assert_eq!(credentials.user, USER);
        assert_eq!(credentials.password, PASSWORD);
        let user_start = HEADER_BYTES + SYSTEM.len();
        assert_eq!(
            credentials.user.as_ptr(),
            copied.as_bytes()[user_start..].as_ptr()
        );
        assert_eq!(
            credentials.password.as_ptr(),
            copied.as_bytes()[user_start + USER.len()..].as_ptr()
        );
    }

    #[test]
    fn every_gateway_round_trips() {
        for gateway in RithmicGateway::ALL {
            let encoded = RithmicCredentialBytes::try_encode(RithmicCredentials {
                gateway,
                ..login(SYSTEM, USER, PASSWORD)
            })
            .expect("login encodes");
            let parsed = encoded.credentials().expect("login parses");
            assert_eq!(parsed.gateway, gateway);
        }
    }

    #[test]
    fn maximum_fields_are_accepted_and_larger_fields_are_rejected() {
        let maximum_system = "s".repeat(MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES);
        let maximum_user = "u".repeat(MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES);
        let maximum_password = "p".repeat(MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES);
        let maximum = encode(&maximum_system, &maximum_user, &maximum_password)
            .expect("maximum login fields encode");
        assert_eq!(
            maximum.as_bytes().len(),
            MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES
        );

        assert_eq!(
            credential_error(encode(&format!("{maximum_system}s"), USER, PASSWORD)),
            RithmicCredentialError::SystemTooLong {
                maximum: MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES,
            }
        );
        assert_eq!(
            credential_error(encode(SYSTEM, &format!("{maximum_user}u"), PASSWORD)),
            RithmicCredentialError::UserTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
        assert_eq!(
            credential_error(encode(SYSTEM, USER, &format!("{maximum_password}p"))),
            RithmicCredentialError::PasswordTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&vec![
                0;
                MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES
                    + 1
            ])),
            RithmicCredentialError::BlobTooLarge {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES,
            }
        );
    }

    #[test]
    fn malformed_headers_lengths_gateways_and_versions_fail_closed() {
        let copy = RithmicCredentialBytes::try_copy_from_vault;
        assert_eq!(
            credential_error(copy(&[])),
            RithmicCredentialError::Truncated
        );
        assert_eq!(
            credential_error(copy(&[1, 0, 1, 0, 1, b'u', b'p'])),
            RithmicCredentialError::UnsupportedVersion
        );
        assert_eq!(
            credential_error(copy(&[2, 99, 0, 1, 0, 1, 0, 1, b's', b'u', b'p'])),
            RithmicCredentialError::UnknownGateway
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 0, 1, 0, 1, b's', b'u'])),
            RithmicCredentialError::LengthMismatch
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 0, 1, 0, 1, b's', b'u', b'p', b'x'])),
            RithmicCredentialError::LengthMismatch
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 1, 0, 0, 1, 0, 1])),
            RithmicCredentialError::SystemTooLong {
                maximum: MAXIMUM_RITHMIC_SYSTEM_NAME_BYTES,
            }
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 1, 1, 0, 1])),
            RithmicCredentialError::UserTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 0, 1, 1, 1])),
            RithmicCredentialError::PasswordTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
    }

    #[test]
    fn empty_invalid_utf8_and_control_fields_are_rejected() {
        assert_eq!(
            credential_error(encode("", USER, PASSWORD)),
            RithmicCredentialError::SystemEmpty
        );
        assert_eq!(
            credential_error(encode(SYSTEM, "", PASSWORD)),
            RithmicCredentialError::UserEmpty
        );
        assert_eq!(
            credential_error(encode(SYSTEM, USER, "")),
            RithmicCredentialError::PasswordEmpty
        );
        assert_eq!(
            credential_error(encode("Rithmic\t01", USER, PASSWORD)),
            RithmicCredentialError::SystemControlCharacter
        );
        assert_eq!(
            credential_error(encode(SYSTEM, "user\n", PASSWORD)),
            RithmicCredentialError::UserControlCharacter
        );
        assert_eq!(
            credential_error(encode(SYSTEM, USER, "pass\rword")),
            RithmicCredentialError::PasswordControlCharacter
        );
        let copy = RithmicCredentialBytes::try_copy_from_vault;
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 0, 1, 0, 1, 0xff, b'u', b'p'])),
            RithmicCredentialError::SystemInvalidUtf8
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 0, 1, 0, 1, b's', 0xff, b'p'])),
            RithmicCredentialError::UserInvalidUtf8
        );
        assert_eq!(
            credential_error(copy(&[2, 1, 0, 1, 0, 1, 0, 1, b's', b'u', 0xff])),
            RithmicCredentialError::PasswordInvalidUtf8
        );
    }

    #[test]
    fn sensitive_debug_and_errors_never_include_credentials() {
        let encoded = encode(SYSTEM, USER, PASSWORD).expect("bounded login encodes");
        let cloned = encoded.clone();
        assert_eq!(encoded.as_bytes(), cloned.as_bytes());
        let debug = format!("{encoded:?} {:?}", encoded.credentials());
        assert!(!debug.contains(USER));
        assert!(!debug.contains(PASSWORD));

        let error =
            encode(SYSTEM, "bad\nuser", PASSWORD).expect_err("control character fails closed");
        let output = format!("{error:?} {error}");
        assert!(!output.contains("bad\nuser"));
        assert!(!output.contains(PASSWORD));
    }
}
