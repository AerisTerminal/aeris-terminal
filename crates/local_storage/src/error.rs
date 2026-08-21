use std::{error::Error, fmt, io};

/// Failures from the desktop-local history boundary.
#[derive(Debug)]
pub enum LocalStorageError {
    InvalidConfiguration(&'static str),
    InvalidIdentity(&'static str),
    CatalogKeyMismatch,
    StoreAlreadyOpen,
    SegmentKeyMismatch,
    CatalogFull { maximum: usize },
    CacheBudgetExceeded { required: u64, maximum: u64 },
    SegmentTooLarge { requested: usize, maximum: usize },
    SegmentAlreadyExists,
    AuthenticationFailed,
    CorruptSegment(&'static str),
    KeyRevocationMissing { key_id: String },
    SharedKeyStillReferenced { key_id: String },
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Random(getrandom::Error),
}

impl fmt::Display for LocalStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => {
                write!(formatter, "invalid storage configuration: {message}")
            }
            Self::InvalidIdentity(field) => {
                write!(formatter, "invalid history identity field: {field}")
            }
            Self::CatalogKeyMismatch => {
                formatter.write_str("catalog key does not match the initialized store")
            }
            Self::StoreAlreadyOpen => {
                formatter.write_str("desktop history root is already open by another process")
            }
            Self::SegmentKeyMismatch => {
                formatter.write_str("segment key does not match the catalog record")
            }
            Self::CatalogFull { maximum } => write!(
                formatter,
                "desktop history catalog reached its {maximum}-entry bound"
            ),
            Self::CacheBudgetExceeded { required, maximum } => write!(
                formatter,
                "protected history requires {required} physical bytes, exceeding the {maximum}-byte cache budget"
            ),
            Self::SegmentTooLarge { requested, maximum } => write!(
                formatter,
                "segment has {requested} bytes, exceeding the {maximum}-byte bound"
            ),
            Self::SegmentAlreadyExists => {
                formatter.write_str("immutable history segment already exists")
            }
            Self::AuthenticationFailed => {
                formatter.write_str("history segment authentication failed")
            }
            Self::CorruptSegment(reason) => {
                write!(formatter, "history segment is corrupt: {reason}")
            }
            Self::KeyRevocationMissing { key_id } => write!(
                formatter,
                "secure deletion lacks OS-vault revocation evidence for key {key_id}"
            ),
            Self::SharedKeyStillReferenced { key_id } => write!(
                formatter,
                "segment key {key_id} is still referenced outside the deletion scope"
            ),
            Self::Io(error) => write!(formatter, "desktop history I/O failed: {error}"),
            Self::Sqlite(error) => write!(formatter, "desktop history catalog failed: {error}"),
            Self::Random(error) => write!(formatter, "secure random generation failed: {error}"),
        }
    }
}

impl Error for LocalStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Random(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for LocalStorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for LocalStorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<getrandom::Error> for LocalStorageError {
    fn from(error: getrandom::Error) -> Self {
        Self::Random(error)
    }
}
