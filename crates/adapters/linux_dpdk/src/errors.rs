use core::fmt;
use std::error::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpdkError {
    FrameLimitUnsupported(usize),
    ReceiveStorageOverflow,
    NativeIntegrationUnavailable,
}

impl fmt::Display for DpdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "DPDK ingest error: {self:?}")
    }
}

impl Error for DpdkError {}
