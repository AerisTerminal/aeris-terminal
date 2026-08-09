use crate::DesktopStorageError;
use std::fmt;
use zeroize::Zeroize;

pub(crate) const MAXIMUM_IDENTITY_BYTES: usize = 192;

/// Provider/account/entitlement boundary that owns one retained segment.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HistoryScope {
    pub provider_id: String,
    pub account_id: String,
    pub entitlement_revision: String,
}

impl HistoryScope {
    pub(crate) fn validate(&self) -> Result<(), DesktopStorageError> {
        validate_identifier("provider_id", &self.provider_id)?;
        validate_identifier("account_id", &self.account_id)?;
        validate_identifier("entitlement_revision", &self.entitlement_revision)
    }
}

/// Immutable history payload class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DataKind {
    Bars,
    Ticks,
    Depth,
    Derived,
}

impl DataKind {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Bars => 1,
            Self::Ticks => 2,
            Self::Depth => 3,
            Self::Derived => 4,
        }
    }
}

/// Complete immutable identity and invalidation dimensions for one segment.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SegmentIdentity {
    pub scope: HistoryScope,
    pub instrument_id: String,
    pub data_kind: DataKind,
    pub resolution: String,
    pub range_start_unix_nanos: i64,
    pub range_end_unix_nanos: i64,
    pub source_revision: u32,
    pub schema_revision: u32,
    pub calendar_revision: u32,
    pub adjustment_revision: u32,
    pub correction_revision: u64,
}

impl SegmentIdentity {
    /// Validates all bounded identity fields and monotonic range/revision values.
    ///
    /// # Errors
    ///
    /// Returns an error naming the invalid identity field.
    pub fn validate(&self) -> Result<(), DesktopStorageError> {
        self.scope.validate()?;
        validate_identifier("instrument_id", &self.instrument_id)?;
        validate_identifier("resolution", &self.resolution)?;
        if self.range_start_unix_nanos >= self.range_end_unix_nanos {
            return Err(DesktopStorageError::InvalidIdentity("time_range"));
        }
        if self.source_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("source_revision"));
        }
        if self.schema_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("schema_revision"));
        }
        if self.calendar_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("calendar_revision"));
        }
        if self.adjustment_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("adjustment_revision"));
        }
        if self.correction_revision == 0 || self.correction_revision > i64::MAX as u64 {
            return Err(DesktopStorageError::InvalidIdentity("correction_revision"));
        }
        Ok(())
    }
}

/// Plaintext dimensions used to discover the newest retained immutable segment.
#[derive(Clone, Copy, Debug)]
pub struct HistorySeriesIdentity<'a> {
    pub scope: &'a HistoryScope,
    pub instrument_id: &'a str,
    pub data_kind: DataKind,
    pub resolution: &'a str,
    pub source_revision: u32,
    pub schema_revision: u32,
    pub calendar_revision: u32,
    pub adjustment_revision: u32,
    pub correction_revision: u64,
}

/// One retained half-open time range for an exact history series revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedRange {
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
}

/// Merged retained coverage for an exact history series revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedSeriesCoverage {
    ranges: Vec<RetainedRange>,
}

impl RetainedSeriesCoverage {
    pub(crate) fn from_ranges(mut ranges: Vec<RetainedRange>) -> Self {
        ranges.sort_unstable_by_key(|range| (range.start_unix_nanos, range.end_unix_nanos));
        let mut merged: Vec<RetainedRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(previous) = merged.last_mut()
                && range.start_unix_nanos <= previous.end_unix_nanos
            {
                previous.end_unix_nanos = previous.end_unix_nanos.max(range.end_unix_nanos);
            } else {
                merged.push(range);
            }
        }
        Self { ranges: merged }
    }

    /// Returns sorted, non-overlapping retained ranges.
    #[must_use]
    pub fn ranges(&self) -> &[RetainedRange] {
        &self.ranges
    }

    /// Returns every missing half-open range inside the requested bounds.
    #[must_use]
    pub fn missing_ranges(&self, requested: RetainedRange) -> Vec<RetainedRange> {
        if requested.start_unix_nanos >= requested.end_unix_nanos {
            return Vec::new();
        }
        let mut missing = Vec::new();
        let mut cursor = requested.start_unix_nanos;
        for range in &self.ranges {
            if range.end_unix_nanos <= cursor {
                continue;
            }
            if range.start_unix_nanos >= requested.end_unix_nanos {
                break;
            }
            if range.start_unix_nanos > cursor {
                missing.push(RetainedRange {
                    start_unix_nanos: cursor,
                    end_unix_nanos: range.start_unix_nanos.min(requested.end_unix_nanos),
                });
            }
            cursor = cursor.max(range.end_unix_nanos);
            if cursor >= requested.end_unix_nanos {
                break;
            }
        }
        if cursor < requested.end_unix_nanos {
            missing.push(RetainedRange {
                start_unix_nanos: cursor,
                end_unix_nanos: requested.end_unix_nanos,
            });
        }
        missing
    }
}

impl HistorySeriesIdentity<'_> {
    pub(crate) fn validate(self) -> Result<(), DesktopStorageError> {
        self.scope.validate()?;
        validate_identifier("instrument_id", self.instrument_id)?;
        validate_identifier("resolution", self.resolution)?;
        if self.source_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("source_revision"));
        }
        if self.schema_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("schema_revision"));
        }
        if self.calendar_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("calendar_revision"));
        }
        if self.adjustment_revision == 0 {
            return Err(DesktopStorageError::InvalidIdentity("adjustment_revision"));
        }
        if self.correction_revision == 0 || self.correction_revision > i64::MAX as u64 {
            return Err(DesktopStorageError::InvalidIdentity("correction_revision"));
        }
        Ok(())
    }
}

/// Provider behavior when retained history is absent or unusable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    ProviderRefetch,
    LiveOnly,
}

impl RecoveryAction {
    pub(crate) const fn code(self) -> i64 {
        match self {
            Self::ProviderRefetch => 1,
            Self::LiveOnly => 2,
        }
    }

    pub(crate) fn from_code(code: i64) -> Result<Self, DesktopStorageError> {
        match code {
            1 => Ok(Self::ProviderRefetch),
            2 => Ok(Self::LiveOnly),
            _ => Err(DesktopStorageError::CorruptSegment(
                "invalid recovery action",
            )),
        }
    }
}

/// Rights-derived local retention rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionPolicy {
    MemoryOnly,
    UntilUnixSeconds(i64),
    UntilRevoked,
}

/// Caller-supplied, versioned segment key loaded from the OS vault.
pub struct SegmentEncryptionKey {
    key_id: String,
    bytes: [u8; 32],
}

impl SegmentEncryptionKey {
    /// Creates a non-cloneable key container that zeroizes its bytes on drop.
    ///
    /// # Errors
    ///
    /// Returns an error when the non-secret vault key identifier is invalid.
    pub fn try_new(key_id: String, bytes: [u8; 32]) -> Result<Self, DesktopStorageError> {
        validate_identifier("segment_key_id", &key_id)?;
        Ok(Self { key_id, bytes })
    }

    /// Returns the non-secret OS-vault key identifier.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) const fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl fmt::Debug for SegmentEncryptionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SegmentEncryptionKey")
            .field("key_id", &self.key_id)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

impl Drop for SegmentEncryptionKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// One bounded immutable publication request.
#[derive(Clone, Copy, Debug)]
pub struct PublicationRequest<'a> {
    pub identity: &'a SegmentIdentity,
    pub payload: &'a [u8],
    pub encryption_key: &'a SegmentEncryptionKey,
    pub retention: RetentionPolicy,
    pub recovery: RecoveryAction,
    pub now_unix_seconds: i64,
}

/// Durable identity returned only after file sync and catalog commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentReceipt {
    pub segment_id: String,
    pub file_name: String,
    pub payload_bytes: u64,
    pub stored_bytes: u64,
    pub storage_checksum: [u8; 32],
}

/// Result of applying current provider retention rights.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationOutcome {
    Published(SegmentReceipt),
    MemoryOnly { recovery: RecoveryAction },
}

/// Why a cache read did not return authoritative retained bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvailabilityReason {
    NotCached,
    Expired,
    Quarantined,
}

/// Rights-aware read result; misses always carry an explicit recovery path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryRead {
    Hit(Vec<u8>),
    Unavailable {
        reason: AvailabilityReason,
        recovery: RecoveryAction,
    },
}

/// Cached authorization and retention facts for one successfully read segment.
#[derive(Clone)]
pub struct SegmentAccessPolicy {
    key_id: String,
    key_verifier: [u8; 32],
    retention_until: Option<i64>,
    recovery: RecoveryAction,
}

impl SegmentAccessPolicy {
    pub(crate) fn new(
        key_id: String,
        key_verifier: [u8; 32],
        retention_until: Option<i64>,
        recovery: RecoveryAction,
    ) -> Self {
        Self {
            key_id,
            key_verifier,
            retention_until,
            recovery,
        }
    }

    /// Revalidates the caller's key and current retention deadline.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied key differs from the retained segment key.
    pub fn validate(
        &self,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<Option<(AvailabilityReason, RecoveryAction)>, DesktopStorageError> {
        if self
            .retention_until
            .is_some_and(|expiry| expiry <= now_unix_seconds)
        {
            return Ok(Some((AvailabilityReason::Expired, self.recovery)));
        }
        if self.key_id != encryption_key.key_id()
            || self.key_verifier != crate::crypto::segment_key_verifier(encryption_key)?
        {
            return Err(DesktopStorageError::SegmentKeyMismatch);
        }
        Ok(None)
    }
}

impl fmt::Debug for SegmentAccessPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SegmentAccessPolicy")
            .field("key_id", &self.key_id)
            .field("key_verifier", &"[REDACTED]")
            .field("retention_until", &self.retention_until)
            .field("recovery", &self.recovery)
            .finish()
    }
}

/// Bounded read result carrying the policy needed for safe memory-cache reuse.
#[derive(Debug)]
pub enum AuthorizedHistoryRead {
    Hit {
        payload: Vec<u8>,
        access_policy: SegmentAccessPolicy,
    },
    Unavailable {
        reason: AvailabilityReason,
        recovery: RecoveryAction,
    },
}

/// Targeted invalidation caused by a changed source dimension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invalidation {
    Entitlement {
        scope: HistoryScope,
    },
    Account {
        provider_id: String,
        account_id: String,
    },
    Schema {
        scope: HistoryScope,
        current_revision: u32,
    },
    Source {
        scope: HistoryScope,
        instrument_id: String,
        current_revision: u32,
    },
    Calendar {
        scope: HistoryScope,
        instrument_id: String,
        current_revision: u32,
    },
    Adjustment {
        scope: HistoryScope,
        instrument_id: String,
        current_revision: u32,
    },
    Correction {
        scope: HistoryScope,
        instrument_id: String,
        current_revision: u64,
    },
}

/// OS-vault evidence required before catalog/file deletion is called secure.
pub trait KeyRevocationEvidence {
    /// Returns whether the named segment key was deleted or made irrecoverable.
    fn confirms_revocation(&self, key_id: &str) -> bool;
}

/// Result of removing one account scope after key revocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeletionReport {
    pub catalog_entries_removed: usize,
    pub files_removed: usize,
    pub physical_remanence_possible: bool,
}

/// Current bounded-catalog state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogStatistics {
    pub active_entries: usize,
    pub quarantined_entries: usize,
    pub coverage_entries: usize,
    pub maximum_entries: usize,
}

pub(crate) fn validate_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), DesktopStorageError> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(DesktopStorageError::InvalidIdentity(field));
    }
    Ok(())
}

#[cfg(test)]
mod coverage_tests {
    use super::{RetainedRange, RetainedSeriesCoverage};

    #[test]
    fn coverage_merges_overlap_and_adjacency_before_gap_detection() {
        let coverage = RetainedSeriesCoverage::from_ranges(vec![
            RetainedRange {
                start_unix_nanos: 30,
                end_unix_nanos: 40,
            },
            RetainedRange {
                start_unix_nanos: 10,
                end_unix_nanos: 20,
            },
            RetainedRange {
                start_unix_nanos: 18,
                end_unix_nanos: 30,
            },
            RetainedRange {
                start_unix_nanos: 50,
                end_unix_nanos: 60,
            },
        ]);
        assert_eq!(
            coverage.ranges(),
            &[
                RetainedRange {
                    start_unix_nanos: 10,
                    end_unix_nanos: 40,
                },
                RetainedRange {
                    start_unix_nanos: 50,
                    end_unix_nanos: 60,
                },
            ]
        );
        assert_eq!(
            coverage.missing_ranges(RetainedRange {
                start_unix_nanos: 0,
                end_unix_nanos: 70,
            }),
            vec![
                RetainedRange {
                    start_unix_nanos: 0,
                    end_unix_nanos: 10,
                },
                RetainedRange {
                    start_unix_nanos: 40,
                    end_unix_nanos: 50,
                },
                RetainedRange {
                    start_unix_nanos: 60,
                    end_unix_nanos: 70,
                },
            ]
        );
    }
}
