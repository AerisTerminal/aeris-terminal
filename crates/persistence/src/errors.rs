//! Persistence boundary failures.

use core::fmt;
use std::error::Error;

/// Reason a persistence operation failed.
#[derive(Debug)]
pub enum PersistenceError {
    Connection(String),
    Query(String),
    MigrationDrift { version: i32 },
    MigrationOrder { expected: i32, actual: i32 },
    InvalidIdentifier,
    PayloadTooLarge(usize),
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "persistence error: {self:?}")
    }
}

impl Error for PersistenceError {}
