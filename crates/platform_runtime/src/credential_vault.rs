//! Protected credential storage boundary.

/// Secure credential storage boundary. Implementations must never use plaintext files.
pub trait CredentialVault {
    type Error;

    /// Stores an opaque secret under a non-secret key.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage is unavailable.
    fn store(&self, key: &str, secret: &[u8]) -> Result<(), Self::Error>;

    /// Loads an opaque secret, if present.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage cannot be queried.
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Deletes an opaque secret.
    ///
    /// # Errors
    ///
    /// Returns an implementation error when protected storage cannot be updated.
    fn delete(&self, key: &str) -> Result<(), Self::Error>;
}
