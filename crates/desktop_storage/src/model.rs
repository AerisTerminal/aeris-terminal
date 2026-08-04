use crate::DesktopStorageError;
use std::fmt;
use zeroize::Zeroize;

pub(crate) const MAXIMUM_IDENTITY_BYTES: usize = 192;

/// Provider/account/entitlement boundary that owns one retained segment.
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub(crate) fn validate(&self) -> Result<(), DesktopStorageError> {
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
