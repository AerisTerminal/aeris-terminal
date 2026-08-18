use std::{error::Error, fmt};

/// Redacted failure at the engine-owned local-history boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHistoryError {
    InvalidRoot,
    InvalidVaultKey,
    InvalidSeries,
    EmptySeries,
    InvalidRange,
    InvalidSegment,
    Unavailable,
}

impl fmt::Display for LocalHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot => formatter.write_str("local history root is invalid"),
            Self::InvalidVaultKey => formatter.write_str("local history vault key is invalid"),
            Self::InvalidSeries => formatter.write_str("local history series identity is invalid"),
            Self::EmptySeries => {
                formatter.write_str("local history cannot persist an empty series")
            }
            Self::InvalidRange => formatter.write_str("local history range is invalid"),
            Self::InvalidSegment => formatter.write_str("local history segment is invalid"),
            Self::Unavailable => formatter.write_str("local history is unavailable"),
        }
    }
}

impl Error for LocalHistoryError {}
