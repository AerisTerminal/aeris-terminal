use std::{error::Error, fmt};

/// Validation and state-transition failures at the provider-history boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderHistoryError {
    InvalidConfiguration(&'static str),
    InvalidIdentity(&'static str),
    UnsupportedDataClass,
    UnsupportedResolution,
    LookbackExceeded,
    RequestSpanExceeded,
    PageLimitExceeded,
    InvalidContinuation,
    QueueFull { maximum: usize },
    InterestAlreadyScheduled { interest: u64 },
    UnknownDispatch { dispatch_id: u64 },
    AbortNotPending { dispatch_id: u64 },
    CompletionMismatch,
    InvalidPage(&'static str),
    LiveBufferFull { maximum: usize },
    SequenceGap { expected: u64, actual: u64 },
    SnapshotRequired,
}

impl fmt::Display for ProviderHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(reason) => {
                write!(formatter, "invalid history configuration: {reason}")
            }
            Self::InvalidIdentity(field) => write!(formatter, "invalid history field: {field}"),
            Self::UnsupportedDataClass => formatter.write_str("history data class is unsupported"),
            Self::UnsupportedResolution => formatter.write_str("history resolution is unsupported"),
            Self::LookbackExceeded => {
                formatter.write_str("history lookback exceeds provider capability")
            }
            Self::RequestSpanExceeded => {
                formatter.write_str("history request span exceeds provider capability")
            }
            Self::PageLimitExceeded => {
                formatter.write_str("history page limit exceeds provider capability")
            }
            Self::InvalidContinuation => {
                formatter.write_str("history continuation does not match provider pagination")
            }
            Self::QueueFull { maximum } => write!(
                formatter,
                "history scheduler reached its {maximum}-request bound"
            ),
            Self::InterestAlreadyScheduled { interest } => write!(
                formatter,
                "history interest {interest} is already scheduled"
            ),
            Self::UnknownDispatch { dispatch_id } => {
                write!(formatter, "unknown history dispatch {dispatch_id}")
            }
            Self::AbortNotPending { dispatch_id } => {
                write!(
                    formatter,
                    "history dispatch {dispatch_id} is not awaiting abort"
                )
            }
            Self::CompletionMismatch => {
                formatter.write_str("history completion does not match its dispatch")
            }
            Self::InvalidPage(reason) => write!(formatter, "invalid history page: {reason}"),
            Self::LiveBufferFull { maximum } => write!(
                formatter,
                "live cutover buffer reached its {maximum}-item bound"
            ),
            Self::SequenceGap { expected, actual } => write!(
                formatter,
                "history sequence gap: expected {expected}, received {actual}"
            ),
            Self::SnapshotRequired => formatter.write_str("a new history snapshot is required"),
        }
    }
}

impl Error for ProviderHistoryError {}
