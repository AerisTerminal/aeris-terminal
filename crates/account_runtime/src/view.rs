//! Sanitized account views the account runtime publishes to the desktop.
//!
//! The runtime owns PKCE transactions, loopback callbacks, token exchange and
//! vault material; presentation receives only browser authorization metadata
//! and sanitized account state.

use std::{fmt, sync::Arc};

/// Runtime-owned account session state shared by all desktop windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum AccountSessionState {
    #[default]
    SignedOut = 0,
    Authorizing = 1,
    Active = 2,
    /// Signed in from the cached verified profile while the identity
    /// provider has not confirmed the session or is unreachable.
    Offline = 3,
    ReauthenticationRequired = 4,
    TerminalError = 5,
}

/// Runtime-issued browser authorization address for one login transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginAuthorization {
    pub request_generation: u64,
    pub authorization_url: String,
    pub expires_unix_seconds: u64,
}

/// Sanitized account state safe for desktop rendering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountView {
    pub state: AccountSessionState,
    pub account_id: String,
    pub plan_id: String,
    pub detail: String,
    pub request_generation: u64,
    pub display_name: String,
    pub email: String,
    pub photo_url: String,
    /// The signed-in profile's photo, fetched by the runtime from `photo_url`.
    pub photo: ProfilePhotoStatus,
}

/// Image encodings the runtime accepts for a profile photo, identified from
/// the downloaded bytes rather than the server's content type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProfilePhotoFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

/// Validated profile photo bytes, shared without copying between views.
#[derive(Clone)]
pub struct ProfilePhoto {
    format: ProfilePhotoFormat,
    bytes: Arc<[u8]>,
}

impl ProfilePhoto {
    /// Largest photo the runtime downloads or accepts.
    pub const MAXIMUM_BYTES: usize = 1024 * 1024;

    /// Accepts bytes that start with a PNG, JPEG, GIF or WebP signature.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes exceed [`Self::MAXIMUM_BYTES`] or are
    /// not one of the accepted encodings.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() > Self::MAXIMUM_BYTES {
            return Err("profile photo exceeds 1 MiB".to_string());
        }
        let format = photo_format(&bytes)
            .ok_or_else(|| "profile photo is not a PNG, JPEG, GIF or WebP image".to_string())?;
        Ok(Self {
            format,
            bytes: bytes.into(),
        })
    }

    #[must_use]
    pub const fn format(&self) -> ProfilePhotoFormat {
        self.format
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl PartialEq for ProfilePhoto {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format
            && (Arc::ptr_eq(&self.bytes, &other.bytes) || self.bytes == other.bytes)
    }
}

impl Eq for ProfilePhoto {}

impl fmt::Debug for ProfilePhoto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfilePhoto")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

fn photo_format(bytes: &[u8]) -> Option<ProfilePhotoFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ProfilePhotoFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ProfilePhotoFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ProfilePhotoFormat::Gif)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ProfilePhotoFormat::Webp)
    } else {
        None
    }
}

/// Whether the signed-in profile's photo is available to render.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProfilePhotoStatus {
    /// No photo: signed out, no fetchable address, or the last fetch failed.
    #[default]
    Absent,
    /// A bounded background fetch for the current address is running.
    Loading,
    Loaded(ProfilePhoto),
}

#[cfg(test)]
mod tests {
    use super::{AccountSessionState, ProfilePhoto, ProfilePhotoFormat};

    #[test]
    fn account_session_states_have_stable_values() {
        assert_eq!(AccountSessionState::SignedOut as i32, 0);
        assert_eq!(AccountSessionState::Authorizing as i32, 1);
        assert_eq!(AccountSessionState::Active as i32, 2);
        assert_eq!(AccountSessionState::Offline as i32, 3);
        assert_eq!(AccountSessionState::ReauthenticationRequired as i32, 4);
        assert_eq!(AccountSessionState::TerminalError as i32, 5);
    }

    #[test]
    fn profile_photos_are_identified_from_their_signature() {
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.extend_from_slice(&[0; 4]);
        for (bytes, format) in [
            (b"\x89PNG\r\n\x1a\n\0\0".to_vec(), ProfilePhotoFormat::Png),
            (vec![0xff, 0xd8, 0xff, 0xe0], ProfilePhotoFormat::Jpeg),
            (b"GIF89a\x01\0".to_vec(), ProfilePhotoFormat::Gif),
            (webp, ProfilePhotoFormat::Webp),
        ] {
            let photo = ProfilePhoto::from_bytes(bytes).expect("supported photo is accepted");
            assert_eq!(photo.format(), format);
        }
    }

    #[test]
    fn profile_photos_reject_other_content_and_oversized_bytes() {
        for bytes in [
            Vec::new(),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
            b"<!doctype html>".to_vec(),
            b"RIFF\0\0\0\0WAVE".to_vec(),
        ] {
            assert!(ProfilePhoto::from_bytes(bytes).is_err());
        }
        let mut oversized = b"GIF89a".to_vec();
        oversized.resize(ProfilePhoto::MAXIMUM_BYTES + 1, 0);
        assert_eq!(
            ProfilePhoto::from_bytes(oversized).expect_err("oversized photo is refused"),
            "profile photo exceeds 1 MiB"
        );
    }

    #[test]
    fn profile_photo_debug_omits_the_image_bytes() {
        let photo = ProfilePhoto::from_bytes(b"GIF89a\x01\0".to_vec()).expect("photo builds");
        assert_eq!(
            format!("{photo:?}"),
            "ProfilePhoto { format: Gif, bytes: 8 }"
        );
    }
}
