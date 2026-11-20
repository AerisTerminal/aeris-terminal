//! Typed internal errors for market-runtime service, auth, and venue seams.

pub use aeris_provider_spi::ProviderAuthError;
use std::fmt;

/// Typed errors for internal coordinator dispatch, shutdown, and venue attachment seams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketServiceError {
    CoordinatorUnavailable,
    CoordinatorBusy,
    CoordinatorStopped,
    DeadlineCancelled,
    DeadlineUnknown,
    ShutdownInProgress,
    ShutdownDeadlineOverflow,
    ShutdownWorkersPanicked(String),
    ShutdownTimedOut(String),
    AlreadyAttached(&'static str),
    WorkerSpawnFailed(String),
    UnsupportedProvider,
    Detail(String),
}

impl fmt::Display for MarketServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoordinatorUnavailable => f.write_str("market engine coordinator is unavailable"),
            Self::CoordinatorBusy => {
                f.write_str("market coordinator is busy; request was not submitted")
            }
            Self::CoordinatorStopped => {
                f.write_str("market engine coordinator stopped before replying")
            }
            Self::DeadlineCancelled => f.write_str(
                "market coordinator deadline expired; request was cancelled before execution",
            ),
            Self::DeadlineUnknown => f.write_str(
                "market coordinator deadline expired after execution started; outcome is unknown",
            ),
            Self::ShutdownInProgress => f.write_str("market engine shutdown is in progress"),
            Self::ShutdownDeadlineOverflow => {
                f.write_str("market engine shutdown deadline overflowed")
            }
            Self::ShutdownWorkersPanicked(names) => {
                write!(f, "market engine workers panicked during shutdown: {names}")
            }
            Self::ShutdownTimedOut(pending) => {
                write!(
                    f,
                    "market engine shutdown deadline expired with active workers: {pending}"
                )
            }
            Self::AlreadyAttached(detail) => f.write_str(detail),
            Self::WorkerSpawnFailed(detail) | Self::Detail(detail) => f.write_str(detail),
            Self::UnsupportedProvider => f.write_str("market provider is unsupported"),
        }
    }
}

impl std::error::Error for MarketServiceError {}

impl From<MarketServiceError> for String {
    fn from(error: MarketServiceError) -> Self {
        error.to_string()
    }
}
