use axiusflow_transport::{DriverLifecycle, ReadinessError};
use core::fmt;
use std::{error::Error, io};

#[derive(Debug)]
pub enum PortableSocketError {
    Readiness(ReadinessError),
    Io(io::Error),
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

impl fmt::Display for PortableSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "portable socket ingest error: {self:?}")
    }
}

impl Error for PortableSocketError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Readiness(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ReadinessError> for PortableSocketError {
    fn from(error: ReadinessError) -> Self {
        Self::Readiness(error)
    }
}
