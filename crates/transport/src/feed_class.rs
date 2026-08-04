//! Provider feed-profile compatibility and readiness vocabulary.

use serde::Serialize;

/// The transport shape exposed by a provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedTransportClass {
    PacketUdp,
    TlsTcpStream,
}

/// Active product profiles for provider messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFeedProfile {
    ProviderWebsocket,
    ProviderNative,
    DeterministicReplay,
    CloudStream,
}

/// One provider profile's current compatibility with a feed class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedProfileCompatibility {
    Applicable,
    TestOnly,
    Unavailable,
}

/// Evaluates one active provider profile against one feed class.
#[must_use]
pub const fn evaluate_profile_feed(
    profile: ProviderFeedProfile,
    feed: FeedTransportClass,
) -> FeedProfileCompatibility {
    match profile {
        ProviderFeedProfile::ProviderWebsocket => match feed {
            FeedTransportClass::TlsTcpStream => FeedProfileCompatibility::Applicable,
            FeedTransportClass::PacketUdp => FeedProfileCompatibility::Unavailable,
        },
        ProviderFeedProfile::ProviderNative | ProviderFeedProfile::CloudStream => {
            FeedProfileCompatibility::Unavailable
        }
        ProviderFeedProfile::DeterministicReplay => FeedProfileCompatibility::TestOnly,
    }
}

/// Human-readable reason for one result, used in evidence.
#[must_use]
pub const fn compatibility_reason(
    profile: ProviderFeedProfile,
    compatibility: FeedProfileCompatibility,
) -> &'static str {
    match (profile, compatibility) {
        (ProviderFeedProfile::ProviderWebsocket, FeedProfileCompatibility::Applicable) => {
            "provider WebSocket profile carries this feed class"
        }
        (ProviderFeedProfile::ProviderWebsocket, FeedProfileCompatibility::Unavailable) => {
            "provider WebSocket profile is incompatible with this feed class"
        }
        (ProviderFeedProfile::ProviderNative, FeedProfileCompatibility::Unavailable) => {
            "provider native profile is unavailable until a provider SDK is qualified"
        }
        (ProviderFeedProfile::DeterministicReplay, FeedProfileCompatibility::TestOnly) => {
            "deterministic replay validates contracts without claiming a live connection"
        }
        (ProviderFeedProfile::CloudStream, FeedProfileCompatibility::Unavailable) => {
            "cloud stream is disabled until the separate cloud-data gate passes"
        }
        _ => "compatibility result does not match the provider profile",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FeedProfileCompatibility, FeedTransportClass, ProviderFeedProfile, compatibility_reason,
        evaluate_profile_feed,
    };

    #[test]
    fn managed_tls_feed_uses_only_the_websocket_profile() {
        assert_eq!(
            evaluate_profile_feed(
                ProviderFeedProfile::ProviderWebsocket,
                FeedTransportClass::TlsTcpStream
            ),
            FeedProfileCompatibility::Applicable
        );
        for profile in [
            ProviderFeedProfile::ProviderNative,
            ProviderFeedProfile::CloudStream,
        ] {
            assert_eq!(
                evaluate_profile_feed(profile, FeedTransportClass::TlsTcpStream),
                FeedProfileCompatibility::Unavailable
            );
        }
        assert_eq!(
            compatibility_reason(
                ProviderFeedProfile::ProviderNative,
                FeedProfileCompatibility::Unavailable
            ),
            "provider native profile is unavailable until a provider SDK is qualified"
        );
        assert_eq!(
            compatibility_reason(
                ProviderFeedProfile::CloudStream,
                FeedProfileCompatibility::Unavailable
            ),
            "cloud stream is disabled until the separate cloud-data gate passes"
        );
    }

    #[test]
    fn websocket_and_unqualified_profiles_reject_packet_udp() {
        for profile in [
            ProviderFeedProfile::ProviderWebsocket,
            ProviderFeedProfile::ProviderNative,
            ProviderFeedProfile::CloudStream,
        ] {
            assert_eq!(
                evaluate_profile_feed(profile, FeedTransportClass::PacketUdp),
                FeedProfileCompatibility::Unavailable
            );
        }
    }

    #[test]
    fn replay_is_test_only_for_every_feed_class() {
        for feed in [
            FeedTransportClass::PacketUdp,
            FeedTransportClass::TlsTcpStream,
        ] {
            assert_eq!(
                evaluate_profile_feed(ProviderFeedProfile::DeterministicReplay, feed),
                FeedProfileCompatibility::TestOnly
            );
        }
        assert!(
            !compatibility_reason(
                ProviderFeedProfile::DeterministicReplay,
                FeedProfileCompatibility::TestOnly
            )
            .is_empty()
        );
    }
}
