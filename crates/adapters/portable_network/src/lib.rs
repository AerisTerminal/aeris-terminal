//! Portable bounded UDP ingest adapter and deterministic fixture target.
//!
//! The native driver owns a fixed set of receive slots selected at queue binding.
//! Borrowed datagram bytes remain valid only until the returned batch is released.

mod config;
mod errors;
mod fixture;
mod native;
#[cfg(test)]
mod test_fixture;

pub use config::PortableSocketConfig;
pub use errors::PortableSocketError;
pub use fixture::fixture_driver;
pub use native::{PortableReceiveBatch, PortableSocketDriver};

use axiusflow_transport::IngestProfile;

pub const PROFILE: IngestProfile = IngestProfile::PortableSocket;

#[cfg(test)]
pub(crate) use config::UDP_MAXIMUM_PAYLOAD_BYTES;
