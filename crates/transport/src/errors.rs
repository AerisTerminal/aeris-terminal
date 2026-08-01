//! Readiness activation rejection reasons.

use crate::profile::{ActiveIngestMode, IngestProfile, ReadinessState};
use core::fmt;
use std::error::Error;

#[derive(Debug)]
pub enum ReadinessError {
    InvalidJson(serde_json::Error),
    UnsupportedSchemaVersion(u32),
    DuplicateProfile(IngestProfile),
    MissingProfile(IngestProfile),
    MissingEvidence(IngestProfile),
    CapabilityProfileMismatch {
        requested: IngestProfile,
        reported: IngestProfile,
    },
    ModeUnavailable(ActiveIngestMode),
    ModeProfileMismatch {
        profile: IngestProfile,
        mode: ActiveIngestMode,
    },
    FixtureCannotProveReadiness(ReadinessState),
    UnreviewedEvidence(String),
    ClaimExceedsEvidence {
        profile: IngestProfile,
        requested: ReadinessState,
        maximum: ReadinessState,
    },
}

impl fmt::Display for ReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ingest readiness rejected: {self:?}")
    }
}

impl Error for ReadinessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            _ => None,
        }
    }
}
