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

/// Runtime-owned account session detail, typed so the desktop localizes it.
///
/// `Failure` carries an already-redacted loopback/OIDC error message; those
/// texts are runtime error payloads and stay in their authored English.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AccountDetail {
    #[default]
    None,
    SignedOut,
    RestoringSavedSignIn,
    SignedInVerifyingSavedSession,
    SignedIn,
    SignedInOfflineReconnecting,
    WaitingForBrowserAuthorization,
    CompletingSignIn,
    SignInCancelled,
    SignInTimedOut,
    SignInWorkerCouldNotStart,
    AccountStateUnavailable,
    CredentialStorageUnavailableSignInAgain,
    CredentialStorageUnavailableRetrySignIn,
    SavedSignInExpired,
    SignInExpired,
    SavedSignInRetireFailed,
    SignInExpiredRetireFailed,
    SignOutCleanupAttention,
    SavedSignInCleanupUnavailable,
    RestoreCouldNotStart,
    SavedSignInUnverifiedConnection,
    SavedSignInUnverified,
    Failure(String),
}

impl AccountDetail {
    /// The authored English wording, used by the runtime-owned browser
    /// outcome page and as the localization reference.
    #[must_use]
    pub fn english(&self) -> &str {
        match self {
            Self::None => "",
            Self::SignedOut => "signed out",
            Self::RestoringSavedSignIn => "restoring saved sign-in",
            Self::SignedInVerifyingSavedSession => "signed in; verifying saved session",
            Self::SignedIn => "signed in",
            Self::SignedInOfflineReconnecting => "signed in offline; reconnecting",
            Self::WaitingForBrowserAuthorization => "waiting for browser authorization",
            Self::CompletingSignIn => "completing sign-in",
            Self::SignInCancelled => "sign-in cancelled",
            Self::SignInTimedOut => "sign-in timed out; retry sign-in",
            Self::SignInWorkerCouldNotStart => "sign-in worker could not start; retry sign-in",
            Self::AccountStateUnavailable => "account state is unavailable",
            Self::CredentialStorageUnavailableSignInAgain => {
                "credential storage is unavailable; sign in again"
            }
            Self::CredentialStorageUnavailableRetrySignIn => {
                "credential storage is unavailable; retry sign-in"
            }
            Self::SavedSignInExpired => "saved sign-in expired; sign in again",
            Self::SignInExpired => "sign-in expired; sign in again",
            Self::SavedSignInRetireFailed => {
                "saved sign-in could not be retired; credential storage is unavailable; sign in again"
            }
            Self::SignInExpiredRetireFailed => {
                "sign-in expired but saved credentials could not be retired; sign in again"
            }
            Self::SignOutCleanupAttention => "signed out; credential cleanup needs attention",
            Self::SavedSignInCleanupUnavailable => {
                "saved sign-in cleanup is unavailable; retry sign-in"
            }
            Self::RestoreCouldNotStart => "saved sign-in restore could not start; sign in again",
            Self::SavedSignInUnverifiedConnection => {
                "saved sign-in could not be verified; check your connection and sign in again"
            }
            Self::SavedSignInUnverified => "saved sign-in could not be verified; sign in again",
            Self::Failure(message) => message.as_str(),
        }
    }

    /// Whether the failure returns the session to a plain signed-out state
    /// instead of a terminal error.
    #[must_use]
    pub fn clears_to_signed_out(&self) -> bool {
        match self {
            Self::SignInTimedOut | Self::SignInCancelled => true,
            // Loopback/OIDC failures arrive as redacted text; the two
            // browser-driven recoverable outcomes keep their wording check.
            Self::Failure(message) => {
                message.contains("timed out") || message.contains("cancelled")
            }
            _ => false,
        }
    }
}
/// Sanitized account state safe for desktop rendering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountView {
    pub state: AccountSessionState,
    pub account_id: String,
    pub plan_id: String,
    pub detail: AccountDetail,
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
    fn account_details_keep_their_authored_english() {
        use super::AccountDetail;
        let cases: [(AccountDetail, &str); 22] = [
            (AccountDetail::SignedOut, "signed out"),
            (
                AccountDetail::RestoringSavedSignIn,
                "restoring saved sign-in",
            ),
            (
                AccountDetail::SignedInVerifyingSavedSession,
                "signed in; verifying saved session",
            ),
            (AccountDetail::SignedIn, "signed in"),
            (
                AccountDetail::SignedInOfflineReconnecting,
                "signed in offline; reconnecting",
            ),
            (
                AccountDetail::WaitingForBrowserAuthorization,
                "waiting for browser authorization",
            ),
            (AccountDetail::CompletingSignIn, "completing sign-in"),
            (AccountDetail::SignInCancelled, "sign-in cancelled"),
            (
                AccountDetail::SignInTimedOut,
                "sign-in timed out; retry sign-in",
            ),
            (
                AccountDetail::SignInWorkerCouldNotStart,
                "sign-in worker could not start; retry sign-in",
            ),
            (
                AccountDetail::AccountStateUnavailable,
                "account state is unavailable",
            ),
            (
                AccountDetail::CredentialStorageUnavailableSignInAgain,
                "credential storage is unavailable; sign in again",
            ),
            (
                AccountDetail::CredentialStorageUnavailableRetrySignIn,
                "credential storage is unavailable; retry sign-in",
            ),
            (
                AccountDetail::SavedSignInExpired,
                "saved sign-in expired; sign in again",
            ),
            (
                AccountDetail::SignInExpired,
                "sign-in expired; sign in again",
            ),
            (
                AccountDetail::SavedSignInRetireFailed,
                "saved sign-in could not be retired; credential storage is unavailable; sign in again",
            ),
            (
                AccountDetail::SignInExpiredRetireFailed,
                "sign-in expired but saved credentials could not be retired; sign in again",
            ),
            (
                AccountDetail::SignOutCleanupAttention,
                "signed out; credential cleanup needs attention",
            ),
            (
                AccountDetail::SavedSignInCleanupUnavailable,
                "saved sign-in cleanup is unavailable; retry sign-in",
            ),
            (
                AccountDetail::RestoreCouldNotStart,
                "saved sign-in restore could not start; sign in again",
            ),
            (
                AccountDetail::SavedSignInUnverifiedConnection,
                "saved sign-in could not be verified; check your connection and sign in again",
            ),
            (
                AccountDetail::SavedSignInUnverified,
                "saved sign-in could not be verified; sign in again",
            ),
        ];
        for (detail, english) in cases {
            assert_eq!(detail.english(), english);
        }
        assert_eq!(AccountDetail::None.english(), "");
        assert_eq!(
            AccountDetail::Failure("redacted failure".to_string()).english(),
            "redacted failure"
        );
    }

    #[test]
    fn only_recoverable_failures_clear_to_signed_out() {
        use super::AccountDetail;
        assert!(AccountDetail::SignInTimedOut.clears_to_signed_out());
        assert!(AccountDetail::SignInCancelled.clears_to_signed_out());
        assert!(
            AccountDetail::Failure("sign-in timed out waiting for the browser".to_string())
                .clears_to_signed_out()
        );
        assert!(
            AccountDetail::Failure("sign-in was declined or cancelled".to_string())
                .clears_to_signed_out()
        );
        assert!(!AccountDetail::SignedOut.clears_to_signed_out());
        assert!(
            !AccountDetail::Failure("sign-in verification failed".to_string())
                .clears_to_signed_out()
        );
    }
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
