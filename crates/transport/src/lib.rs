//! Transport-neutral bounded ingest contracts and runtime readiness enforcement.

mod binary_frame;
mod driver;
mod errors;
mod feed_class;
mod fixture_driver;
mod profile;
mod readiness;
mod receive_batch;

pub use binary_frame::{
    BINARY_FRAME_LENGTH_BYTES, BinaryFrameError, BoundedBinaryFrameDecoder, encode_binary_frame,
};
pub use driver::{
    DriverCapabilities, DriverHealth, DriverLifecycle, IngestDriver, QueueBinding,
    software_fixture_capabilities,
};
pub use errors::ReadinessError;
pub use feed_class::{
    FeedProfileCompatibility, FeedTransportClass, ProviderFeedProfile, compatibility_reason,
    evaluate_profile_feed,
};
pub use fixture_driver::{
    FixtureDriverError, FixtureFrame, FixtureIngestDriver, FixtureReceiveBatch,
};
pub use profile::{ActiveIngestMode, IngestProfile, ReadinessState};
pub use readiness::{ActivationPermit, ActivationRequest, ProfileReadiness, ReadinessManifest};
pub use receive_batch::{
    BorrowedFrame, OverflowReport, ReceiveBatch, ReceiveMetadata, TimestampSource,
};
