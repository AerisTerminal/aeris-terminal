//! Streaming boundary failures.

use core::fmt;
use std::error::Error;

/// Reason a durable streaming operation failed.
#[derive(Debug)]
pub enum StreamingError {
    InvalidEventId,
    InvalidProducer,
    InvalidTopicSegment,
    PayloadTooLarge(usize),
    EnvelopeTooLarge(usize),
    EnvelopeTruncated,
    InvalidTimestamp,
    UnsupportedPlatform,
    Client(String),
    QueueFull,
    Delivery(String),
    Timeout,
}

impl fmt::Display for StreamingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "streaming error: {self:?}")
    }
}

impl Error for StreamingError {}
