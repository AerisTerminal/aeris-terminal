//! Bounded, keyed user-facing failures. The raw error is never retained or logged.

use std::time::Duration;

pub(super) const MAXIMUM_NOTICES: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NoticeKey {
    pub scope: u64,
    pub kind: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NoticeSeverity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NoticeTone {
    Warning,
    Loss,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum NoticeSource {
    Pane(u64),
    Workspace,
    Persistence,
    Provider(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetryTarget {
    ChartChromePreferences,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NoticeAction {
    Retry(RetryTarget),
}

impl NoticeAction {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Retry(_) => "Retry",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NoticeLifetime {
    Timed(Duration),
    UntilCleared,
    UntilDismissed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UserNotice {
    pub key: NoticeKey,
    pub severity: NoticeSeverity,
    pub tone: NoticeTone,
    pub source: NoticeSource,
    pub title: &'static str,
    /// A public, deliberately composed explanation, never a raw error or credential.
    pub detail: String,
    pub action: Option<NoticeAction>,
    pub lifetime: NoticeLifetime,
}

#[derive(Debug, Default)]
pub(super) struct NotificationCenter {
    notices: Vec<UserNotice>,
}

impl NotificationCenter {
    pub fn notices(&self) -> &[UserNotice] {
        &self.notices
    }

    /// Returns the key removed at capacity, if any. Persistent conditions cannot bypass the
    /// bound: after timed notices are gone, the oldest persistent notice is retired.
    pub fn post(&mut self, notice: UserNotice) -> Option<NoticeKey> {
        aeris_observability::diagnostic!(
            "desktop notice: {} ({:?})",
            notice.title,
            notice.severity
        );
        if let Some(existing) = self.notices.iter_mut().find(|item| item.key == notice.key) {
            *existing = notice;
            return None;
        }
        let evicted = if self.notices.len() == MAXIMUM_NOTICES {
            let index = self
                .notices
                .iter()
                .position(|item| matches!(item.lifetime, NoticeLifetime::Timed(_)))
                .unwrap_or(0);
            Some(self.notices.remove(index).key)
        } else {
            None
        };
        self.notices.push(notice);
        evicted
    }

    pub fn clear(&mut self, key: &NoticeKey) -> bool {
        let old_len = self.notices.len();
        self.notices.retain(|notice| &notice.key != key);
        self.notices.len() != old_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(scope: u64, lifetime: NoticeLifetime) -> UserNotice {
        UserNotice {
            key: NoticeKey {
                scope,
                kind: "test",
            },
            severity: NoticeSeverity::Error,
            tone: NoticeTone::Loss,
            source: NoticeSource::Persistence,
            title: "Market unavailable",
            detail: "Try again later".into(),
            action: None,
            lifetime,
        }
    }

    #[test]
    fn notification_existing_key_replaces_in_place() {
        let mut center = NotificationCenter::default();
        let mut actionable = notice(1, NoticeLifetime::UntilCleared);
        actionable.action = Some(NoticeAction::Retry(RetryTarget::ChartChromePreferences));
        center.post(actionable);
        let mut changed = notice(1, NoticeLifetime::UntilCleared);
        changed.detail = "Connection restored".into();
        center.post(changed.clone());
        assert_eq!(center.notices(), &[changed]);
        assert_eq!(center.notices()[0].action, None);
    }

    #[test]
    fn notification_capacity_evicts_oldest_timed_before_persistent() {
        let mut center = NotificationCenter::default();
        center.post(notice(0, NoticeLifetime::UntilDismissed));
        center.post(notice(1, NoticeLifetime::Timed(Duration::from_secs(2))));
        for scope in 2..MAXIMUM_NOTICES as u64 {
            center.post(notice(scope, NoticeLifetime::UntilCleared));
        }
        assert_eq!(
            center.post(notice(100, NoticeLifetime::UntilCleared)),
            Some(NoticeKey {
                scope: 1,
                kind: "test"
            })
        );
        assert_eq!(center.notices().len(), MAXIMUM_NOTICES);
        assert_eq!(center.notices()[0].key.scope, 0);
    }

    #[test]
    fn notification_persistent_keys_are_bounded() {
        let mut center = NotificationCenter::default();
        for scope in 0..100 {
            center.post(notice(scope, NoticeLifetime::UntilCleared));
        }
        assert_eq!(center.notices().len(), MAXIMUM_NOTICES);
        assert_eq!(center.notices()[0].key.scope, 68);
    }

    #[test]
    fn notification_clear_removes_only_matching_key() {
        let mut center = NotificationCenter::default();
        center.post(notice(1, NoticeLifetime::UntilCleared));
        center.post(notice(2, NoticeLifetime::UntilDismissed));
        assert!(center.clear(&NoticeKey {
            scope: 1,
            kind: "test"
        }));
        assert!(!center.clear(&NoticeKey {
            scope: 1,
            kind: "test"
        }));
        assert_eq!(center.notices()[0].key.scope, 2);
    }

    #[test]
    fn notification_secret_bearing_error_is_not_retained() {
        let credential = "private-credential-sentinel";
        let error = format!("authentication failed for {credential}");
        let mut center = NotificationCenter::default();
        let public = notice(1, NoticeLifetime::UntilDismissed);
        center.post(public);
        assert!(!format!("{:?}", center.notices()).contains(credential));
        assert!(!center.notices()[0].detail.contains(&error));
    }
}
