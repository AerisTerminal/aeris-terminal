//! Bounded operating-system user-notification delivery.

use std::{
    fmt,
    sync::{OnceLock, mpsc},
    thread,
};

const NOTIFICATION_CAPACITY: usize = 32;
const MAXIMUM_TITLE_BYTES: usize = 128;
const MAXIMUM_BODY_BYTES: usize = 1_024;

/// Sanitized notification content accepted by the native delivery worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeUserNotification {
    title: String,
    body: String,
}

impl NativeUserNotification {
    /// Creates bounded title and body content.
    ///
    /// # Errors
    /// Returns an error when either field is empty or exceeds its byte bound.
    pub fn try_new(
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, NativeUserNotificationError> {
        let title = title.into();
        let body = body.into();
        if title.is_empty() || title.len() > MAXIMUM_TITLE_BYTES {
            return Err(NativeUserNotificationError::InvalidContent);
        }
        if body.is_empty() || body.len() > MAXIMUM_BODY_BYTES {
            return Err(NativeUserNotificationError::InvalidContent);
        }
        Ok(Self { title, body })
    }
}

/// Redacted failure to enqueue a native user notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUserNotificationError {
    InvalidContent,
    WorkerUnavailable,
    QueueFull,
}

impl fmt::Display for NativeUserNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidContent => "native notification content is invalid",
            Self::WorkerUnavailable => "native notification worker is unavailable",
            Self::QueueFull => "native notification queue is full",
        })
    }
}

impl std::error::Error for NativeUserNotificationError {}

struct NotificationDispatcher {
    sender: mpsc::SyncSender<NativeUserNotification>,
}

static DISPATCHER: OnceLock<Result<NotificationDispatcher, NativeUserNotificationError>> =
    OnceLock::new();

/// Enqueues an operating-system notification without blocking the caller.
///
/// A single process-wide worker performs native delivery. The queue is bounded
/// and rejects overload explicitly rather than creating one task per alert.
///
/// # Errors
/// Returns an error when the worker cannot start, has stopped, or its bounded
/// queue is full.
pub fn try_send_user_notification(
    notification: NativeUserNotification,
) -> Result<(), NativeUserNotificationError> {
    let dispatcher = DISPATCHER
        .get_or_init(start_dispatcher)
        .as_ref()
        .map_err(|error| *error)?;
    dispatcher
        .sender
        .try_send(notification)
        .map_err(|error| match error {
            mpsc::TrySendError::Full(_) => NativeUserNotificationError::QueueFull,
            mpsc::TrySendError::Disconnected(_) => NativeUserNotificationError::WorkerUnavailable,
        })
}

fn start_dispatcher() -> Result<NotificationDispatcher, NativeUserNotificationError> {
    let (sender, receiver) = mpsc::sync_channel(NOTIFICATION_CAPACITY);
    thread::Builder::new()
        .name("axiusflow-user-notifications".to_string())
        .spawn(move || notification_worker(&receiver))
        .map_err(|_| NativeUserNotificationError::WorkerUnavailable)?;
    Ok(NotificationDispatcher { sender })
}

fn notification_worker(receiver: &mpsc::Receiver<NativeUserNotification>) {
    while let Ok(notification) = receiver.recv() {
        let mut native = notify_rust::Notification::new();
        native
            .appname("Axiusflow")
            .summary(&notification.title)
            .body(&notification.body)
            .timeout(notify_rust::Timeout::Milliseconds(10_000));
        #[cfg(target_os = "windows")]
        native.app_id("com.axiusflow.desktop").sound_name("Default");
        #[cfg(target_os = "macos")]
        native.sound_name("Default");
        if let Err(error) = native.show() {
            eprintln!("Axiusflow operating-system notification failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_content_is_bounded_before_enqueue() {
        assert!(NativeUserNotification::try_new("Price alert", "BTC crossed 100").is_ok());
        assert_eq!(
            NativeUserNotification::try_new("", "body"),
            Err(NativeUserNotificationError::InvalidContent)
        );
        assert_eq!(
            NativeUserNotification::try_new("title", "x".repeat(MAXIMUM_BODY_BYTES + 1)),
            Err(NativeUserNotificationError::InvalidContent)
        );
    }
}
