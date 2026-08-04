//! Rights-aware desktop-local history storage.
//!
//! The blocking API is intended for bounded storage workers, never the GPUI
//! thread. Provider credentials and encryption keys remain outside `SQLite` and
//! are supplied from the platform credential-vault boundary.

mod catalog;
mod crypto;
mod error;
mod model;
mod store;

pub use error::DesktopStorageError;
pub use model::{
    AuthorizedHistoryRead, AvailabilityReason, CatalogStatistics, DataKind, DeletionReport,
    HistoryRead, HistoryScope, Invalidation, KeyRevocationEvidence, PublicationOutcome,
    PublicationRequest, RecoveryAction, RetentionPolicy, SegmentAccessPolicy, SegmentEncryptionKey,
    SegmentIdentity, SegmentReceipt,
};
pub use store::{CatalogKey, HistoryStore, MAXIMUM_CATALOG_ENTRIES, MAXIMUM_SEGMENT_BYTES};
