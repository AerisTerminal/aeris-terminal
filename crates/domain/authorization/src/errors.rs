//! Validation failures rejected before policy evaluation.

use core::fmt;
use std::error::Error;

/// Validation failures rejected before policy evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationValidationError {
    EmptyIdentifier(&'static str),
    InvalidIdentifier(&'static str),
    IdentifierTooLong {
        field: &'static str,
        requested: usize,
        maximum: usize,
    },
    EmptyCorrelationId,
    InvalidCorrelationId,
    CorrelationIdTooLong {
        requested: usize,
        maximum: usize,
    },
    ZeroPolicyVersion,
    GrantLimitExceeded {
        requested: usize,
        maximum: usize,
    },
    DuplicateGrantId,
    DuplicateGrantKey,
}

impl fmt::Display for AuthorizationValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentifier(field) => write!(formatter, "{field} must not be empty"),
            Self::InvalidIdentifier(field) => {
                write!(
                    formatter,
                    "{field} contains bytes outside the visible ASCII range"
                )
            }
            Self::IdentifierTooLong {
                field,
                requested,
                maximum,
            } => write!(
                formatter,
                "{field} contains {requested} bytes; maximum is {maximum}"
            ),
            Self::EmptyCorrelationId => formatter.write_str("correlation_id must not be empty"),
            Self::InvalidCorrelationId => {
                formatter.write_str("correlation_id contains bytes outside the visible ASCII range")
            }
            Self::CorrelationIdTooLong { requested, maximum } => write!(
                formatter,
                "correlation_id contains {requested} bytes; maximum is {maximum}"
            ),
            Self::ZeroPolicyVersion => formatter.write_str("policy version must be non-zero"),
            Self::GrantLimitExceeded { requested, maximum } => write!(
                formatter,
                "policy snapshot contains {requested} grants; maximum is {maximum}"
            ),
            Self::DuplicateGrantId => {
                formatter.write_str("policy snapshot contains a duplicate grant_id")
            }
            Self::DuplicateGrantKey => formatter
                .write_str("policy snapshot contains duplicate principal/resource/action grants"),
        }
    }
}

impl Error for AuthorizationValidationError {}
