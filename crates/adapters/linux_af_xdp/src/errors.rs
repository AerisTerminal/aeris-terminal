//! `AF_XDP` activation and lifecycle rejection reasons.

use axiusflow_transport::{DriverLifecycle, ReadinessError};
use core::fmt;
use std::error::Error;

#[derive(Debug)]
pub enum AfXdpError {
    EmptyInterfaceName,
    FrameLimitUnsupported(usize),
    ReceiveStorageOverflow,
    RingSizeUnsupported(usize),
    QueueUnavailable(u16),
    BatchLimitUnsupported(usize),
    QueueNotBound,
    BatchOutstanding,
    InvalidLifecycle {
        expected: DriverLifecycle,
        actual: DriverLifecycle,
    },
    NativeIntegrationUnavailable,
    Readiness(ReadinessError),
    NativeOpen(String),
    NativeReceive(String),
    DescriptorRecycle(String),
}

impl From<ReadinessError> for AfXdpError {
    fn from(error: ReadinessError) -> Self {
        Self::Readiness(error)
    }
}

impl fmt::Display for AfXdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AF_XDP ingest error: {self:?}")
    }
}

impl Error for AfXdpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Readiness(error) => Some(error),
            _ => None,
        }
    }
}
