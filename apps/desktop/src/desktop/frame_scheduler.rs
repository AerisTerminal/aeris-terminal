//! Dirty-source frame scheduling for a terminal window.
//!
//! Every producer that has work for the UI thread marks its source in the window's
//! [`DirtySet`] from whatever thread it runs on, which wakes the one listener that requests
//! a frame. The frame takes the marks and drains only the marked sources, each within its
//! existing budget; a source that still has work after its budget marks itself again, which
//! requests the next frame. Nothing is polled for a source that has not marked itself, so an
//! idle window schedules no frames.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::frame_poll_gate::FramePollGate;
use super::{UiWake, UiWakeNotified};

/// A window-level producer of UI work. Chart panes are marked separately by identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AppSource {
    /// The market-summary workers behind the tab and watchlist quotes.
    Summaries,
    /// The market-screener worker.
    Screener,
    /// The trading subscription published a newer working set.
    Trading,
    /// Trading requests completed, and their outcomes become notices.
    Notices,
    /// The workspace writer finished a save.
    PersistenceReceipts,
    /// The account presentation changed.
    Account,
    /// The update client's maintenance interval elapsed.
    Updater,
}

impl AppSource {
    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct PaneMarks {
    all: bool,
    listed: BTreeSet<u64>,
}

/// The sources and panes marked since the last frame took them.
///
/// The pane set holds at most the panes that woke since that frame, so it is bounded by the
/// window's open panes plus panes retired in between.
pub(super) struct DirtySet {
    sources: AtomicU32,
    panes: Mutex<PaneMarks>,
    wake: UiWake,
}

impl DirtySet {
    fn new() -> Self {
        Self {
            sources: AtomicU32::new(0),
            panes: Mutex::new(PaneMarks::default()),
            wake: UiWake::default(),
        }
    }

    /// Marks `source` from any thread and wakes the frame listener.
    pub(super) fn mark(&self, source: AppSource) {
        self.sources.fetch_or(source.bit(), Ordering::AcqRel);
        self.wake.wake();
    }

    /// Marks the pane whose surface entity is `pane` from any thread.
    pub(super) fn mark_pane(&self, pane: u64) {
        self.panes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .listed
            .insert(pane);
        self.wake.wake();
    }

    /// Marks every pane, for a producer shared by all of them.
    pub(super) fn mark_all_panes(&self) {
        self.panes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .all = true;
        self.wake.wake();
    }

    /// Marks every source and pane, so the frame drains whatever arrived before its
    /// producers could mark themselves.
    fn mark_everything(&self) {
        self.sources.store(u32::MAX, Ordering::Release);
        self.mark_all_panes();
    }

    /// A producer callback that marks `source`.
    pub(super) fn source_wake(self: &Arc<Self>, source: AppSource) -> Arc<dyn Fn() + Send + Sync> {
        let set = Arc::clone(self);
        Arc::new(move || set.mark(source))
    }

    /// A producer callback that marks one pane.
    pub(super) fn pane_wake(self: &Arc<Self>, pane: u64) -> Arc<dyn Fn() + Send + Sync> {
        let set = Arc::clone(self);
        Arc::new(move || set.mark_pane(pane))
    }

    /// A producer callback that marks every pane.
    pub(super) fn all_panes_wake(self: &Arc<Self>) -> Arc<dyn Fn() + Send + Sync> {
        let set = Arc::clone(self);
        Arc::new(move || set.mark_all_panes())
    }

    #[cfg(test)]
    fn is_clean(&self) -> bool {
        self.sources.load(Ordering::Acquire) == 0
            && *self.panes.lock().unwrap_or_else(PoisonError::into_inner) == PaneMarks::default()
    }

    /// Resolves once anything has been marked since the last wake.
    pub(super) fn notified(&self) -> UiWakeNotified {
        self.wake.notified()
    }

    fn take(&self) -> DirtyWork {
        let panes = std::mem::take(&mut *self.panes.lock().unwrap_or_else(PoisonError::into_inner));
        DirtyWork {
            sources: self.sources.swap(0, Ordering::AcqRel),
            panes,
        }
    }
}

/// What one frame drains.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct DirtyWork {
    sources: u32,
    panes: PaneMarks,
}

impl DirtyWork {
    pub(super) const fn contains(&self, source: AppSource) -> bool {
        self.sources & source.bit() != 0
    }

    pub(super) fn includes_pane(&self, pane: u64) -> bool {
        self.panes.all || self.panes.listed.contains(&pane)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Listeners {
    NotStarted,
    Started,
}

/// The window's frame boundary: its dirty set, the one requested frame, the one-time
/// listener start and the layout revision the workspace writer has accepted.
pub(super) struct FrameScheduler {
    dirty: Arc<DirtySet>,
    frame: FramePollGate,
    listeners: Listeners,
    layout_revision: u64,
    persisted_revision: u64,
}

impl Default for FrameScheduler {
    fn default() -> Self {
        Self {
            dirty: Arc::new(DirtySet::new()),
            frame: FramePollGate::default(),
            listeners: Listeners::NotStarted,
            // The restored workspace is one revision the writer has not accepted: a restore
            // that changed the layout library is written back on the first frame.
            layout_revision: 1,
            persisted_revision: 0,
        }
    }
}

impl FrameScheduler {
    pub(super) const fn dirty(&self) -> &Arc<DirtySet> {
        &self.dirty
    }

    /// Reports true exactly once, for the caller that starts the listeners. Everything is
    /// marked then, so the first frame drains work queued before its producer was wired.
    pub(super) fn claim_listener_start(&mut self) -> bool {
        if self.listeners == Listeners::Started {
            return false;
        }
        self.listeners = Listeners::Started;
        self.dirty.mark_everything();
        true
    }

    /// Claims the next frame, reporting false while one is already requested.
    pub(super) const fn try_request_frame(&mut self) -> bool {
        self.frame.try_schedule()
    }

    /// The requested frame runs: a later mark may request another, and this frame drains
    /// what was marked until now.
    pub(super) fn begin_frame(&mut self) -> DirtyWork {
        self.frame.complete();
        self.dirty.take()
    }

    /// Nothing is marked and no frame is requested.
    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        !self.frame.is_scheduled() && self.dirty.is_clean()
    }

    /// Records a change to the persisted workspace.
    pub(super) const fn layout_changed(&mut self) {
        self.layout_revision = self.layout_revision.wrapping_add(1);
    }

    /// The layout revision the writer has not accepted yet, if any.
    pub(super) const fn layout_save_due(&self) -> Option<u64> {
        if self.layout_revision == self.persisted_revision {
            None
        } else {
            Some(self.layout_revision)
        }
    }

    /// The writer accepted the workspace as of `revision`.
    pub(super) const fn layout_saved(&mut self, revision: u64) {
        self.persisted_revision = revision;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_takes_only_the_marked_sources_and_panes() {
        let mut frames = FrameScheduler::default();
        frames.dirty().mark(AppSource::Trading);
        frames.dirty().mark_pane(7);
        let work = frames.begin_frame();
        assert!(work.contains(AppSource::Trading));
        assert!(!work.contains(AppSource::Summaries));
        assert!(work.includes_pane(7));
        assert!(!work.includes_pane(8));
        let idle = frames.begin_frame();
        assert!(!idle.contains(AppSource::Trading));
        assert!(!idle.includes_pane(7));
    }

    #[test]
    fn producer_callbacks_mark_from_another_thread() {
        let mut frames = FrameScheduler::default();
        let source = frames.dirty().source_wake(AppSource::PersistenceReceipts);
        let pane = frames.dirty().pane_wake(3);
        std::thread::spawn(move || {
            source();
            pane();
        })
        .join()
        .expect("producer thread");
        let work = frames.begin_frame();
        assert!(work.contains(AppSource::PersistenceReceipts));
        assert!(work.includes_pane(3));
    }

    #[test]
    fn a_shared_producer_marks_every_pane() {
        let mut frames = FrameScheduler::default();
        (frames.dirty().all_panes_wake())();
        let work = frames.begin_frame();
        assert!(work.includes_pane(1));
        assert!(work.includes_pane(64));
        assert!(!work.contains(AppSource::Account));
    }

    #[test]
    fn a_burst_of_marks_requests_one_frame_until_it_runs() {
        let mut frames = FrameScheduler::default();
        assert!(frames.try_request_frame());
        frames.dirty().mark_pane(1);
        frames.dirty().mark_pane(2);
        assert!(!frames.try_request_frame());
        let work = frames.begin_frame();
        assert!(work.includes_pane(1) && work.includes_pane(2));
        assert!(frames.is_idle());
        assert!(frames.try_request_frame());
        assert!(!frames.is_idle());
    }

    #[test]
    fn a_mark_during_a_frame_is_left_for_the_next_one() {
        let mut frames = FrameScheduler::default();
        assert!(frames.try_request_frame());
        let first = frames.begin_frame();
        assert!(!first.includes_pane(5));
        frames.dirty().mark_pane(5);
        assert!(frames.try_request_frame());
        assert!(frames.begin_frame().includes_pane(5));
    }

    #[test]
    fn the_listeners_start_once_and_mark_everything() {
        let mut frames = FrameScheduler::default();
        assert!(frames.claim_listener_start());
        assert!(!frames.claim_listener_start());
        let work = frames.begin_frame();
        for source in [
            AppSource::Summaries,
            AppSource::Screener,
            AppSource::Trading,
            AppSource::Notices,
            AppSource::PersistenceReceipts,
            AppSource::Account,
            AppSource::Updater,
        ] {
            assert!(work.contains(source), "{source:?}");
        }
        assert!(work.includes_pane(42));
    }

    #[test]
    fn the_layout_is_saved_once_per_revision() {
        let mut frames = FrameScheduler::default();
        let restored = frames.layout_save_due().expect("the restore is unsaved");
        frames.layout_saved(restored);
        assert_eq!(frames.layout_save_due(), None);
        frames.layout_changed();
        frames.layout_changed();
        let revision = frames.layout_save_due().expect("one save owed");
        frames.layout_saved(revision);
        assert_eq!(frames.layout_save_due(), None);
    }

    #[test]
    fn an_unaccepted_save_stays_due() {
        let mut frames = FrameScheduler::default();
        frames.layout_changed();
        let revision = frames.layout_save_due().expect("save owed");
        assert_eq!(frames.layout_save_due(), Some(revision));
        frames.layout_changed();
        assert_ne!(frames.layout_save_due(), Some(revision));
    }
}
