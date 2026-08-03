//! Feed-transport-class compatibility for ingest profiles.
//!
//! Section 2.7: a provider delivered only through TLS/TCP or a managed API does
//! not become faster merely by selecting a kernel-bypass profile. This module
//! makes that rule executable: every profile reports one explicit compatibility
//! result for each feed class, and accelerated profiles report `Unavailable`
//! for feed classes they cannot honestly carry.

use crate::profile::IngestProfile;
use serde::Serialize;

/// The transport shape of a provider feed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedTransportClass {
    PacketUdp,
    TlsTcpStream,
}

/// One profile's compatibility with one feed class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedProfileCompatibility {
    Applicable,
    NoBenefit,
    Unavailable,
}

/// Evaluates one profile against one feed class.
#[must_use]
pub const fn evaluate_profile_feed(
    profile: IngestProfile,
    feed: FeedTransportClass,
) -> FeedProfileCompatibility {
    match profile {
        IngestProfile::PortableSocket => FeedProfileCompatibility::Applicable,
        IngestProfile::TunedLinuxSocket => match feed {
            FeedTransportClass::PacketUdp => FeedProfileCompatibility::Applicable,
            FeedTransportClass::TlsTcpStream => FeedProfileCompatibility::NoBenefit,
        },
        IngestProfile::LinuxAfXdp | IngestProfile::LinuxDpdk => match feed {
            FeedTransportClass::PacketUdp => FeedProfileCompatibility::Applicable,
            FeedTransportClass::TlsTcpStream => FeedProfileCompatibility::Unavailable,
        },
    }
}

/// Human-readable reason for one result, used in evidence.
#[must_use]
pub const fn compatibility_reason(compatibility: FeedProfileCompatibility) -> &'static str {
    match compatibility {
        FeedProfileCompatibility::Applicable => "carries this feed class",
        FeedProfileCompatibility::NoBenefit => {
            "UDP tuning knobs do not apply to a TLS/TCP stream; no latency benefit"
        }
        FeedProfileCompatibility::Unavailable => {
            "kernel-bypass packet path cannot terminate TLS; activation fails explicitly"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FeedProfileCompatibility, FeedTransportClass, compatibility_reason, evaluate_profile_feed,
    };
    use crate::IngestProfile;

    #[test]
    fn tls_feed_is_unavailable_on_kernel_bypass_profiles() {
        for profile in [IngestProfile::LinuxAfXdp, IngestProfile::LinuxDpdk] {
            assert_eq!(
                evaluate_profile_feed(profile, FeedTransportClass::TlsTcpStream),
                FeedProfileCompatibility::Unavailable
            );
        }
        assert_eq!(
            evaluate_profile_feed(IngestProfile::PortableSocket, FeedTransportClass::TlsTcpStream),
            FeedProfileCompatibility::Applicable
        );
        assert_eq!(
            evaluate_profile_feed(
                IngestProfile::TunedLinuxSocket,
                FeedTransportClass::TlsTcpStream
            ),
            FeedProfileCompatibility::NoBenefit
        );
        assert!(!compatibility_reason(FeedProfileCompatibility::Unavailable).is_empty());
    }

    #[test]
    fn packet_udp_feed_remains_applicable_on_socket_profiles() {
        for profile in [IngestProfile::PortableSocket, IngestProfile::TunedLinuxSocket] {
            assert_eq!(
                evaluate_profile_feed(profile, FeedTransportClass::PacketUdp),
                FeedProfileCompatibility::Applicable
            );
        }
        assert_eq!(
            evaluate_profile_feed(IngestProfile::LinuxAfXdp, FeedTransportClass::PacketUdp),
            FeedProfileCompatibility::Applicable
        );
    }
}
