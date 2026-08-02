//! Credential and session state in `PostgreSQL`.
//!
//! Passwords are stored as argon2 PHC strings; verification of an unknown
//! principal still runs one full argon2 verification against a fixed dummy hash
//! so lookup timing does not enumerate accounts. Session secrets are stored as
//! SHA-256 digests, never in plaintext, and revocation is a durable timestamp.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use sha2::{Digest, Sha256};

/// Binds the workspace's shared argon2 hasher.
pub struct CredentialStore<'client> {
    client: &'client mut postgres::Client,
}

/// One verified authentication session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRecord {
    pub session_id: String,
    pub principal_id: String,
    pub expires_at_unix_nanos: i64,
}

/// Dummy PHC string verified when the principal does not exist, keeping
/// unknown-account timing on the same argon2 path.
const DUMMY_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0c2FsdA$WM1UiYPgMiU6YQ+OvAKM5X0Z4SzMqLWK+4s35hW8qyk";
/// Password bounds enforced at creation.
const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 256;
const MAX_PRINCIPAL_BYTES: usize = 256;
const SESSION_LIFETIME_NANOS: i64 = 86_400_000_000_000;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 900;

impl<'client> CredentialStore<'client> {
    /// Borrows one open `PostgreSQL` client.
    pub const fn new(client: &'client mut postgres::Client) -> Self {
        Self { client }
    }

    /// Creates one credential with a fresh argon2 hash.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, duplicates, or database failures.
    pub fn create_credential(&mut self, principal_id: &str, password: &str) -> Result<(), String> {
        validate_principal(principal_id)?;
        validate_password(password)?;
        let salt = random_salt()?;
        let phc = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|error| format!("password hashing failed: {error}"))?
            .to_string();
        self.client
            .execute(
                "insert into auth_credentials (principal_id, argon2_phc, created_at_unix_nanos)
                values ($1, $2, $3)",
                &[&principal_id, &phc, &unix_nanos_now()],
            )
            .map_err(|error| format!("credential insert failed: {error}"))?;
        Ok(())
    }

    /// Verifies a password and opens one session on success.
    ///
    /// # Errors
    ///
    /// Returns an error for database failures; verification failure returns
    /// `Ok(None)` with no distinguishing detail.
    pub fn open_session(
        &mut self,
        principal_id: &str,
        password: &str,
    ) -> Result<Option<(SessionRecord, String)>, String> {
        validate_principal(principal_id)?;
        validate_password(password)?;
        let stored: Option<String> = self
            .client
            .query_opt(
                "select argon2_phc from auth_credentials where principal_id = $1",
                &[&principal_id],
            )
            .map_err(|error| format!("credential lookup failed: {error}"))?
            .map(|row| row.get(0));
        let phc = stored.as_deref().unwrap_or(DUMMY_PHC);
        let parsed = PasswordHash::new(phc).map_err(|_| "stored credential is malformed")?;
        let verified = Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        if !verified || stored.is_none() {
            return Ok(None);
        }

        let session_id = random_identifier("session")?;
        let session_secret = random_secret()?;
        let digest = hex_sha256(session_secret.as_bytes());
        let now = unix_nanos_now();
        let expires = now + SESSION_LIFETIME_NANOS;
        self.client
            .execute(
                "insert into auth_sessions
                    (session_id, principal_id, session_secret_sha256,
                     created_at_unix_nanos, expires_at_unix_nanos)
                values ($1, $2, $3, $4, $5)",
                &[&session_id, &principal_id, &digest, &now, &expires],
            )
            .map_err(|error| format!("session insert failed: {error}"))?;
        Ok(Some((
            SessionRecord {
                session_id,
                principal_id: principal_id.to_string(),
                expires_at_unix_nanos: expires,
            },
            session_secret,
        )))
    }

    /// Loads a live session by secret; used by the session endpoint milestone
    /// and by the integration lane.
    #[allow(dead_code)]
    ///
    /// # Errors
    ///
    /// Returns an error for database failures.
    pub fn load_session(&mut self, session_secret: &str) -> Result<Option<SessionRecord>, String> {
        let digest = hex_sha256(session_secret.as_bytes());
        let now = unix_nanos_now();
        let row = self
            .client
            .query_opt(
                "select session_id, principal_id, expires_at_unix_nanos
                from auth_sessions
                where session_secret_sha256 = $1
                  and revoked_at_unix_nanos is null
                  and expires_at_unix_nanos > $2",
                &[&digest, &now],
            )
            .map_err(|error| format!("session lookup failed: {error}"))?;
        Ok(row.map(|row| SessionRecord {
            session_id: row.get(0),
            principal_id: row.get(1),
            expires_at_unix_nanos: row.get(2),
        }))
    }

    /// Revokes one session durably; used by the sign-out endpoint milestone
    /// and by the integration lane.
    #[allow(dead_code)]
    ///
    /// # Errors
    ///
    /// Returns an error for database failures.
    pub fn revoke_session(&mut self, session_id: &str) -> Result<(), String> {
        self.client
            .execute(
                "update auth_sessions set revoked_at_unix_nanos = $2
                where session_id = $1 and revoked_at_unix_nanos is null",
                &[&session_id, &unix_nanos_now()],
            )
            .map_err(|error| format!("session revoke failed: {error}"))?;
        Ok(())
    }
}

/// Maximum token lifetime this service will issue.
pub const fn maximum_token_lifetime_seconds() -> u64 {
    MAX_TOKEN_LIFETIME_SECONDS
}

fn validate_principal(principal_id: &str) -> Result<(), String> {
    if principal_id.is_empty()
        || principal_id.len() > MAX_PRINCIPAL_BYTES
        || !principal_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '@')
        })
    {
        return Err("invalid principal id".to_string());
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.len() < MIN_PASSWORD_BYTES || password.len() > MAX_PASSWORD_BYTES {
        return Err("password length outside policy bounds".to_string());
    }
    Ok(())
}

fn random_salt() -> Result<SaltString, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    SaltString::from_b64(&base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        bytes,
    ))
    .map_err(|error| error.to_string())
}

fn random_identifier(prefix: &str) -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(format!("{prefix}_{}", hex(&bytes)))
}

fn random_secret() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(hex(&bytes))
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{byte:02x}"));
    }
    output
}

fn unix_nanos_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{DUMMY_PHC, validate_password, validate_principal};
    use argon2::password_hash::PasswordHash;

    #[test]
    fn dummy_phc_is_a_valid_argon2_string() {
        PasswordHash::new(DUMMY_PHC).expect("the dummy hash parses");
    }

    #[test]
    fn password_policy_bounds() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("a]very long password within bounds").is_ok());
    }

    #[test]
    fn principal_validation() {
        assert!(validate_principal("user-1@example.com").is_ok());
        assert!(validate_principal("").is_err());
        assert!(validate_principal("user name").is_err());
    }
}
