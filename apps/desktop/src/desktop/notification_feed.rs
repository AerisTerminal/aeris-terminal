//! The session's notification history behind the sidebar bell: what the terminal announced
//! as toasts (fills, order and workflow notices, triggered price alerts and platform notices),
//! newest first, with how many arrived since the trader last looked. Chart conditions are
//! states rather than announcements and are not recorded. The history lives only in memory.

use std::collections::VecDeque;
use std::time::Instant;

use gpui::SharedString;

use super::toaster::ToastTone;

/// Announcements the history keeps; the oldest gives way past this.
pub(super) const MAXIMUM_FEED_ENTRIES: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FeedEntry {
    pub(super) tone: ToastTone,
    pub(super) title: SharedString,
    pub(super) detail: Option<SharedString>,
    /// When the terminal announced it; [`entry_wall_unix_nanos`] turns it into wall time.
    pub(super) recorded: Instant,
}

#[derive(Debug, Default)]
pub(super) struct NotificationFeed {
    /// Newest first.
    entries: VecDeque<FeedEntry>,
    unread: usize,
}

impl NotificationFeed {
    pub(super) fn record(&mut self, entry: FeedEntry) {
        if self.entries.len() == MAXIMUM_FEED_ENTRIES {
            self.entries.pop_back();
        }
        self.entries.push_front(entry);
        self.unread = (self.unread + 1).min(self.entries.len());
    }

    /// Newest first.
    pub(super) fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.entries.iter()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) const fn unread(&self) -> usize {
        self.unread
    }

    /// Reports whether anything was unread.
    pub(super) fn mark_read(&mut self) -> bool {
        std::mem::take(&mut self.unread) != 0
    }

    /// Forgets the history, reporting whether it held anything.
    pub(super) fn clear(&mut self) -> bool {
        self.unread = 0;
        let had_entries = !self.entries.is_empty();
        self.entries.clear();
        had_entries
    }
}

/// The wall time `recorded` happened at, from a wall-clock reading taken at `now`; the
/// toaster keeps monotonic instants and has no wall clock of its own.
pub(super) fn entry_wall_unix_nanos(
    recorded: Instant,
    now: Instant,
    now_wall_unix_nanos: i64,
) -> i64 {
    let elapsed =
        i64::try_from(now.saturating_duration_since(recorded).as_nanos()).unwrap_or(i64::MAX);
    now_wall_unix_nanos.saturating_sub(elapsed)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn entry(title: &str, recorded: Instant) -> FeedEntry {
        FeedEntry {
            tone: ToastTone::Muted,
            title: SharedString::from(title.to_string()),
            detail: None,
            recorded,
        }
    }

    fn titles(feed: &NotificationFeed) -> Vec<String> {
        feed.entries()
            .map(|entry| entry.title.to_string())
            .collect()
    }

    #[test]
    fn the_feed_lists_newest_first_and_stays_bounded() {
        let now = Instant::now();
        let mut feed = NotificationFeed::default();
        for index in 0..MAXIMUM_FEED_ENTRIES + 5 {
            feed.record(entry(&index.to_string(), now));
        }
        let titles = titles(&feed);
        assert_eq!(titles.len(), MAXIMUM_FEED_ENTRIES);
        assert_eq!(titles[0], (MAXIMUM_FEED_ENTRIES + 4).to_string());
        assert_eq!(titles[MAXIMUM_FEED_ENTRIES - 1], "5", "the oldest gave way");
        assert_eq!(
            feed.unread(),
            MAXIMUM_FEED_ENTRIES,
            "unread never exceeds the history"
        );
    }

    #[test]
    fn reading_and_clearing_reset_the_unread_count() {
        let now = Instant::now();
        let mut feed = NotificationFeed::default();
        assert!(!feed.mark_read());
        feed.record(entry("fill", now));
        feed.record(entry("alert", now));
        assert_eq!(feed.unread(), 2);
        assert!(feed.mark_read());
        assert_eq!(feed.unread(), 0);
        assert_eq!(
            titles(&feed),
            ["alert", "fill"],
            "reading keeps the history"
        );
        feed.record(entry("notice", now));
        assert_eq!(feed.unread(), 1);
        assert!(feed.clear());
        assert!(feed.is_empty());
        assert_eq!(feed.unread(), 0);
        assert!(!feed.clear());
    }

    #[test]
    fn an_entry_reads_at_the_wall_time_it_was_announced() {
        let recorded = Instant::now();
        let now = recorded + Duration::from_secs(90);
        assert_eq!(
            entry_wall_unix_nanos(recorded, now, 1_000_000_000_000),
            1_000_000_000_000 - 90_000_000_000
        );
        assert_eq!(
            entry_wall_unix_nanos(now, recorded, 5),
            5,
            "an entry is never later than now"
        );
    }
}
