use crate::RithmicCredentials;
use core::fmt;
use zeroize::Zeroize;

const CREDENTIAL_VERSION: u8 = 1;
const HEADER_BYTES: usize = 5;

/// Maximum UTF-8 bytes accepted in either Rithmic credential field.
pub const MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES: usize = 256;

/// Maximum bytes accepted in one versioned Rithmic credential blob.
pub const MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES: usize =
    HEADER_BYTES + (2 * MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES);

/// Redacted validation failures for a versioned Rithmic credential blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicCredentialError {
    BlobTooLarge { maximum: usize },
    Truncated,
    UnsupportedVersion,
    LengthMismatch,
    UserEmpty,
    PasswordEmpty,
    UserTooLong { maximum: usize },
    PasswordTooLong { maximum: usize },
    UserInvalidUtf8,
    PasswordInvalidUtf8,
    UserControlCharacter,
    PasswordControlCharacter,
}

impl fmt::Display for RithmicCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic credential blob is invalid: {self:?}")
    }
}

impl std::error::Error for RithmicCredentialError {}

/// Owned credential bytes that zeroize every attempt-local copy on drop.
pub struct RithmicCredentialBytes(Vec<u8>);

impl RithmicCredentialBytes {
    /// Encodes validated credentials into the version-1 vault representation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for empty, oversized, or control-containing fields.
    pub fn try_encode(user: &str, password: &str) -> Result<Self, RithmicCredentialError> {
        validate_text_fields(user, password)?;
        let user_length =
            u16::try_from(user.len()).map_err(|_| RithmicCredentialError::UserTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            })?;
        let password_length =
            u16::try_from(password.len()).map_err(|_| RithmicCredentialError::PasswordTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            })?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES + user.len() + password.len());
        bytes.push(CREDENTIAL_VERSION);
        bytes.extend_from_slice(&user_length.to_be_bytes());
        bytes.extend_from_slice(&password_length.to_be_bytes());
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

    /// Returns borrowed credentials without allocating additional strings.
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
    let header = bytes
        .get(..HEADER_BYTES)
        .ok_or(RithmicCredentialError::Truncated)?;
    if header[0] != CREDENTIAL_VERSION {
        return Err(RithmicCredentialError::UnsupportedVersion);
    }
    let user_length = usize::from(u16::from_be_bytes([header[1], header[2]]));
    let password_length = usize::from(u16::from_be_bytes([header[3], header[4]]));
    if user_length > MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES {
        return Err(RithmicCredentialError::UserTooLong {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
        });
    }
    if password_length > MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES {
        return Err(RithmicCredentialError::PasswordTooLong {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
        });
    }
    let expected_length = HEADER_BYTES
        .checked_add(user_length)
        .and_then(|length| length.checked_add(password_length))
        .ok_or(RithmicCredentialError::LengthMismatch)?;
    if bytes.len() != expected_length {
        return Err(RithmicCredentialError::LengthMismatch);
    }
    let user_end = HEADER_BYTES + user_length;
    let user = std::str::from_utf8(&bytes[HEADER_BYTES..user_end])
        .map_err(|_| RithmicCredentialError::UserInvalidUtf8)?;
    let password = std::str::from_utf8(&bytes[user_end..])
        .map_err(|_| RithmicCredentialError::PasswordInvalidUtf8)?;
    validate_text_fields(user, password)?;
    Ok(RithmicCredentials { user, password })
}

fn validate_text_fields(user: &str, password: &str) -> Result<(), RithmicCredentialError> {
    if user.is_empty() {
        return Err(RithmicCredentialError::UserEmpty);
    }
    if password.is_empty() {
        return Err(RithmicCredentialError::PasswordEmpty);
    }
    if user.len() > MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES {
        return Err(RithmicCredentialError::UserTooLong {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
        });
    }
    if password.len() > MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES {
        return Err(RithmicCredentialError::PasswordTooLong {
            maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
        });
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

    const USER: &str = "credential-fixture-user";
    const PASSWORD: &str = "credential-fixture-password";

    fn credential_error(
        result: Result<RithmicCredentialBytes, RithmicCredentialError>,
    ) -> RithmicCredentialError {
        result.expect_err("invalid credential blob fails closed")
    }

    #[test]
    fn version_one_round_trip_is_big_endian_and_borrowed() {
        let encoded =
            RithmicCredentialBytes::try_encode(USER, PASSWORD).expect("bounded credentials encode");
        let bytes = encoded.as_bytes();
        assert_eq!(bytes[0], 1);
        assert_eq!(
            &bytes[1..3],
            &u16::try_from(USER.len())
                .expect("fixture user length fits")
                .to_be_bytes()
        );
        assert_eq!(
            &bytes[3..5],
            &u16::try_from(PASSWORD.len())
                .expect("fixture password length fits")
                .to_be_bytes()
        );

        let copied = RithmicCredentialBytes::try_copy_from_vault(bytes)
            .expect("vault bytes validate and copy");
        let credentials = copied.credentials().expect("copied credentials parse");
        assert_eq!(credentials.user, USER);
        assert_eq!(credentials.password, PASSWORD);
        assert_eq!(credentials.user.as_ptr(), copied.as_bytes()[5..].as_ptr());
        assert_eq!(
            credentials.password.as_ptr(),
            copied.as_bytes()[5 + USER.len()..].as_ptr()
        );
    }

    #[test]
    fn maximum_fields_are_accepted_and_larger_fields_are_rejected() {
        let maximum_user = "u".repeat(MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES);
        let maximum_password = "p".repeat(MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES);
        let maximum = RithmicCredentialBytes::try_encode(&maximum_user, &maximum_password)
            .expect("maximum credential fields encode");
        assert_eq!(
            maximum.as_bytes().len(),
            MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES
        );

        let oversized_user = format!("{maximum_user}u");
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_encode(
                &oversized_user,
                PASSWORD,
            )),
            RithmicCredentialError::UserTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
        let oversized_password = format!("{maximum_password}p");
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_encode(
                USER,
                &oversized_password,
            )),
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
    fn malformed_headers_lengths_and_versions_fail_closed() {
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[])),
            RithmicCredentialError::Truncated
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                2, 0, 1, 0, 1, b'u', b'p',
            ])),
            RithmicCredentialError::UnsupportedVersion
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                1, 0, 1, 0, 1, b'u',
            ])),
            RithmicCredentialError::LengthMismatch
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                1, 0, 1, 0, 1, b'u', b'p', b'x',
            ])),
            RithmicCredentialError::LengthMismatch
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                1, 1, 1, 0, 1,
            ])),
            RithmicCredentialError::UserTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                1, 0, 1, 1, 1,
            ])),
            RithmicCredentialError::PasswordTooLong {
                maximum: MAXIMUM_RITHMIC_CREDENTIAL_FIELD_BYTES,
            }
        );
    }

    #[test]
    fn empty_invalid_utf8_and_control_fields_are_rejected() {
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_encode("", PASSWORD)),
            RithmicCredentialError::UserEmpty
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_encode(USER, "")),
            RithmicCredentialError::PasswordEmpty
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_encode("user\n", PASSWORD)),
            RithmicCredentialError::UserControlCharacter
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_encode(USER, "pass\rword")),
            RithmicCredentialError::PasswordControlCharacter
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                1, 0, 1, 0, 1, 0xff, b'p',
            ])),
            RithmicCredentialError::UserInvalidUtf8
        );
        assert_eq!(
            credential_error(RithmicCredentialBytes::try_copy_from_vault(&[
                1, 0, 1, 0, 1, b'u', 0xff,
            ])),
            RithmicCredentialError::PasswordInvalidUtf8
        );
    }

    #[test]
    fn sensitive_debug_and_errors_never_include_credentials() {
        let encoded =
            RithmicCredentialBytes::try_encode(USER, PASSWORD).expect("bounded credentials encode");
        let cloned = encoded.clone();
        assert_eq!(encoded.as_bytes(), cloned.as_bytes());
        let debug = format!("{encoded:?}");
        assert!(!debug.contains(USER));
        assert!(!debug.contains(PASSWORD));

        let error = RithmicCredentialBytes::try_encode("bad\nuser", PASSWORD)
            .expect_err("control character fails closed");
        let output = format!("{error:?} {error}");
        assert!(!output.contains("bad\nuser"));
        assert!(!output.contains(PASSWORD));
    }
}
