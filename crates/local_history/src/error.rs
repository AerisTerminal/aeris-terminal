use axiusflow_local_storage::LocalStorageError;
use axiusflow_provider_history::ProviderHistoryError;
use std::{error::Error, fmt};

/// Failure at the worker-owned desktop history boundary.
#[derive(Debug)]
pub enum LocalHistoryError {
    InvalidConfiguration(&'static str),
    UiThreadWorkForbidden,
    WorkerThreadMismatch,
    Decode(String),
    DecodedHistoryTooLarge { requested: usize, maximum: usize },
    CacheFull { maximum_entries: usize },
    ChartLimitReached { maximum: usize },
    HandoffAlreadyStarted,
    HandoffLimitReached { maximum: usize },
    MissingHandoff,
    Storage(LocalStorageError),
    Provider(ProviderHistoryError),
}

impl fmt::Display for LocalHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(reason) => {
                write!(formatter, "invalid desktop history configuration: {reason}")
            }
            Self::UiThreadWorkForbidden => formatter.write_str(
                "desktop history storage, provider, and decode work is forbidden on the UI thread",
            ),
            Self::WorkerThreadMismatch => {
                formatter.write_str("desktop history worker used from a non-owner thread")
            }
            Self::Decode(reason) => write!(formatter, "desktop history decode failed: {reason}"),
            Self::DecodedHistoryTooLarge { requested, maximum } => write!(
                formatter,
                "decoded history requires {requested} bytes, exceeding the {maximum}-byte bound"
            ),
            Self::CacheFull { maximum_entries } => write!(
                formatter,
                "desktop history cache reached its {maximum_entries}-entry bound"
            ),
            Self::ChartLimitReached { maximum } => {
                write!(
                    formatter,
                    "desktop history cache reached its {maximum}-chart bound"
                )
            }
            Self::HandoffAlreadyStarted => formatter.write_str("history handoff is already active"),
            Self::HandoffLimitReached { maximum } => {
                write!(
                    formatter,
                    "desktop history reached its {maximum}-handoff bound"
                )
            }
            Self::MissingHandoff => formatter.write_str("history handoff has not been started"),
            Self::Storage(error) => write!(formatter, "desktop history storage failed: {error}"),
            Self::Provider(error) => write!(formatter, "desktop history handoff failed: {error}"),
        }
    }
}

impl Error for LocalHistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Provider(error) => Some(error),
            _ => None,
        }
    }
}

impl From<LocalStorageError> for LocalHistoryError {
    fn from(error: LocalStorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<ProviderHistoryError> for LocalHistoryError {
    fn from(error: ProviderHistoryError) -> Self {
        Self::Provider(error)
    }
}
