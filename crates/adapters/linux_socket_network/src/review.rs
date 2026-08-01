//! Recorded tuning review text and profile constants.

use axiusflow_transport::IngestProfile;

pub const PROFILE: IngestProfile = IngestProfile::TunedLinuxSocket;
pub const TUNING_REVIEW: &str =
    "socket2 0.6.5; MIT OR Apache-2.0; rust-lang maintained; safe public API; Linux-only link";
pub const BUSY_POLL_ENABLED: bool = false;
pub const HARDWARE_TIMESTAMP_VERIFIED: bool = false;

#[cfg(target_os = "linux")]
pub const TUNED_HOST_MODE_IMPLEMENTED: bool = true;
#[cfg(not(target_os = "linux"))]
pub const TUNED_HOST_MODE_IMPLEMENTED: bool = false;

#[cfg(test)]
mod tests {
    use super::PROFILE;
    use axiusflow_transport::IngestProfile;

    #[test]
    fn profile_is_tuned_linux_socket() {
        assert_eq!(PROFILE, IngestProfile::TunedLinuxSocket);
    }
}
