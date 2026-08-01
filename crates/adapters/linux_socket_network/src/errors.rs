//! Tuned Linux socket activation and I/O rejection reasons.

use axiusflow_transport::{DriverLifecycle, ReadinessError};
use core::fmt;
use std::{error::Error, io};

#[derive(Debug)]
pub enum TunedLinuxSocketError {
    Readiness(ReadinessError),
    Io(io::Error),
    UnsupportedPlatform,
    ZeroPollTimeout,
    InvalidIdlePollInterval,
    ReceiveStorageOverflow,
    QueueNotBound,
    NotStarted,
    BatchOutstanding,
    QueueUnavailable(u16),
    BatchLimitUnsupported(usize),
    FrameLimitUnsupported(usize),
    InvalidLifecycle {
        expected: DriverLifecycle,
        actual: DriverLifecycle,
    },
}

impl fmt::Display for TunedLinuxSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "tuned Linux socket ingest error: {self:?}")
    }
}

impl Error for TunedLinuxSocketError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Readiness(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ReadinessError> for TunedLinuxSocketError {
    fn from(error: ReadinessError) -> Self {
        Self::Readiness(error)
    }
}
