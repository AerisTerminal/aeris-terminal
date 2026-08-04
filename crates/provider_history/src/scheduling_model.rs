use crate::{Continuation, DataClass, HistoryPage, HistoryPageRequest};
use std::{
    collections::BTreeSet,
    num::{NonZeroU64, NonZeroUsize},
};

/// Caller-owned cancellation identity shared by visible and prefetch requests.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestInterest(NonZeroU64);

impl RequestInterest {
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Dispatch ordering. Lower-value variants are selected first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RequestPriority {
    Visible,
    AdjacentPrefetch,
    Background,
}

/// Scheduler memory and fan-out bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerConfig {
    pub maximum_queued_requests: NonZeroUsize,
    pub maximum_total_inflight: NonZeroUsize,
    pub maximum_interests_per_request: NonZeroUsize,
    pub maximum_continuations_per_request: NonZeroUsize,
    pub maximum_fetch_attempts: NonZeroUsize,
    pub adjacent_prefetch_windows: usize,
}

/// Result of adding a visible request and derived adjacent windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Submission {
    pub queued_new: usize,
    pub deduplicated: usize,
    pub adjacent_prefetches: usize,
}

/// One request released to a provider worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dispatch {
    pub dispatch_id: u64,
    pub request: HistoryPageRequest,
    pub priority: RequestPriority,
}

/// One dispatch attempt plus queued requests that expired before dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchOutcome {
    pub dispatch: Option<Dispatch>,
    pub expired: Vec<ExpiredRequest>,
}

/// An expired request and the consumers that must be notified or rescheduled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpiredRequest {
    pub request: HistoryPageRequest,
    pub interests: Vec<RequestInterest>,
}

/// Result of canceling all queued/inflight work for one interest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelOutcome {
    pub queued_removed: usize,
    pub provider_aborts: Vec<u64>,
}

/// Result of finalizing one provider fetch failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FetchFailureOutcome {
    Requeued {
        attempts: usize,
    },
    Terminal {
        attempts: usize,
        interests: Vec<RequestInterest>,
    },
}

/// Validated provider page plus its interested consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    pub(crate) page: HistoryPage,
    pub(crate) interests: Vec<RequestInterest>,
    pub(crate) continuation_scheduled: bool,
}

impl Completion {
    #[must_use]
    pub const fn page(&self) -> &HistoryPage {
        &self.page
    }

    #[must_use]
    pub fn interests(&self) -> &[RequestInterest] {
        &self.interests
    }

    #[must_use]
    pub const fn continuation_scheduled(&self) -> bool {
        self.continuation_scheduled
    }
}

#[derive(Clone)]
pub(crate) struct QueueEntry {
    pub request: HistoryPageRequest,
    pub priority: RequestPriority,
    pub order: u64,
    pub interests: BTreeSet<RequestInterest>,
    pub continuations: BTreeSet<Continuation>,
    pub accepted_at_unix_nanos: i64,
    pub attempts: usize,
}

#[derive(Clone)]
pub(crate) struct InflightEntry {
    pub request: HistoryPageRequest,
    pub priority: RequestPriority,
    pub interests: BTreeSet<RequestInterest>,
    pub continuations: BTreeSet<Continuation>,
    pub accepted_at_unix_nanos: i64,
    pub order: u64,
    pub attempts: usize,
    pub aborting: bool,
}

pub(crate) fn class_inflight(
    inflight: impl Iterator<Item = DataClass>,
    data_class: DataClass,
) -> usize {
    inflight
        .filter(|candidate| *candidate == data_class)
        .count()
}
