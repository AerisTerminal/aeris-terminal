use aeris_provider_history::{
    CancelOutcome, Continuation, CoverageClass, CoverageSnapshot, DataClass, DatasetCapability,
    FetchFailureOutcome, HistoryCapabilities, HistoryItem, HistoryPage, HistoryPageRequest,
    HistoryRange, HistoryScheduler, PaginationStyle, ProviderHistoryError, RateLimit,
    RequestInterest, RequestPriority, SchedulerConfig,
};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

const NOW: i64 = 10_000;
const MONOTONIC_NOW: u64 = 1_000;

fn nonzero_u32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

fn nonzero_u64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test value is non-zero")
}

fn nonzero_usize(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test value is non-zero")
}

fn capabilities(
    pagination: PaginationStyle,
    requests_per_window: u32,
    maximum_inflight: usize,
) -> HistoryCapabilities {
    let supported = |resolutions: &[&str]| {
        DatasetCapability::supported(
            resolutions.iter().map(|value| (*value).to_string()),
            nonzero_u64(10_000),
            nonzero_u64(1_000),
            nonzero_usize(16),
            pagination,
            RateLimit {
                requests: nonzero_u32(requests_per_window),
                window_nanos: nonzero_u64(100),
                maximum_inflight: nonzero_usize(maximum_inflight),
            },
        )
        .expect("fixture capability is valid")
    };
    HistoryCapabilities::try_new(
        "fixture".to_string(),
        supported(&["1m", "5m"]),
        supported(&["tick"]),
        DatasetCapability::unsupported("depth history unavailable"),
    )
    .expect("fixture matrix is valid")
}

fn request(instrument: &str, range: HistoryRange) -> HistoryPageRequest {
    HistoryPageRequest {
        provider_id: "fixture".to_string(),
        account_id: "account-a".to_string(),
        entitlement_revision: "rights-1".to_string(),
        instrument_id: instrument.to_string(),
        data_class: DataClass::Bars,
        resolution: "1m".to_string(),
        range,
        maximum_items: nonzero_usize(8),
        continuation: None,
    }
}

fn make_scheduler(
    pagination: PaginationStyle,
    requests_per_window: u32,
    maximum_inflight: usize,
    maximum_queued: usize,
    prefetches: usize,
) -> HistoryScheduler {
    HistoryScheduler::try_new(
        capabilities(pagination, requests_per_window, maximum_inflight),
        SchedulerConfig {
            maximum_queued_requests: nonzero_usize(maximum_queued),
            maximum_total_inflight: nonzero_usize(8),
            maximum_interests_per_request: nonzero_usize(8),
            maximum_continuations_per_request: nonzero_usize(8),
            maximum_fetch_attempts: nonzero_usize(2),
            adjacent_prefetch_windows: prefetches,
        },
    )
    .expect("fixture scheduler is valid")
}

fn interest(value: u64) -> RequestInterest {
    RequestInterest::new(nonzero_u64(value))
}

fn item(sequence: u64, event_time: i64) -> HistoryItem {
    HistoryItem {
        sequence,
        event_time_unix_nanos: event_time,
        payload: vec![u8::try_from(sequence).unwrap_or(u8::MAX)],
    }
}

#[test]
fn coverage_plan_classifies_every_span_and_repairs_only_unusable_ranges() {
    let snapshot = CoverageSnapshot::try_new(
        vec![HistoryRange {
            start_unix_nanos: 100,
            end_unix_nanos: 200,
        }],
        vec![HistoryRange {
            start_unix_nanos: 200,
            end_unix_nanos: 250,
        }],
        vec![HistoryRange {
            start_unix_nanos: 250,
            end_unix_nanos: 300,
        }],
        vec![HistoryRange {
            start_unix_nanos: 300,
            end_unix_nanos: 350,
        }],
    )
    .expect("coverage facts validate");
    let plan = snapshot
        .plan(HistoryRange {
            start_unix_nanos: 50,
            end_unix_nanos: 400,
        })
        .expect("requested coverage validates");
    assert_eq!(plan.classification(), CoverageClass::Partial);
    assert_eq!(
        plan.spans()
            .iter()
            .map(|span| span.class)
            .collect::<Vec<_>>(),
        vec![
            CoverageClass::Missing,
            CoverageClass::Complete,
            CoverageClass::ConfirmedEmpty,
            CoverageClass::Invalidated,
            CoverageClass::Quarantined,
            CoverageClass::Missing,
        ]
    );
    assert_eq!(
        plan.repair_ranges(),
        &[
            HistoryRange {
                start_unix_nanos: 50,
                end_unix_nanos: 100,
            },
            HistoryRange {
                start_unix_nanos: 250,
                end_unix_nanos: 400,
            },
        ]
    );
}

#[test]
fn coverage_repairs_dispatch_visible_gaps_first_and_deduplicate_interests() {
    let snapshot = CoverageSnapshot::try_new(
        vec![HistoryRange {
            start_unix_nanos: 9_200,
            end_unix_nanos: 9_400,
        }],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("coverage facts validate");
    let requested = HistoryRange {
        start_unix_nanos: 9_000,
        end_unix_nanos: 9_900,
    };
    let plan = snapshot.plan(requested).expect("coverage plan");
    let base = request("btc-usd", requested);
    let mut scheduler = make_scheduler(PaginationStyle::None, 2, 2, 8, 0);
    let submitted = scheduler
        .submit_coverage_repairs(
            &base,
            &plan,
            HistoryRange {
                start_unix_nanos: 9_500,
                end_unix_nanos: 9_600,
            },
            interest(1),
            NOW,
        )
        .expect("repair work queues");
    assert_eq!(submitted.queued_new, 2);
    assert_eq!(submitted.adjacent_prefetches, 1);
    let first = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("dispatch succeeds")
        .dispatch
        .expect("visible repair dispatches");
    assert_eq!(first.priority, RequestPriority::Visible);
    assert_eq!(
        first.request.range,
        HistoryRange {
            start_unix_nanos: 9_400,
            end_unix_nanos: 9_900,
        }
    );
    let duplicate = scheduler
        .submit_coverage_repairs(
            &base,
            &plan,
            HistoryRange {
                start_unix_nanos: 9_500,
                end_unix_nanos: 9_600,
            },
            interest(2),
            NOW,
        )
        .expect("second interest shares repair work");
    assert_eq!(duplicate.queued_new, 0);
    assert_eq!(duplicate.deduplicated, 2);
}

#[test]
fn capability_matrix_rejects_unsupported_and_out_of_bound_requests() {
    let mut scheduler = make_scheduler(PaginationStyle::None, 2, 2, 8, 0);
    let base = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_500,
            end_unix_nanos: 9_900,
        },
    );
    scheduler
        .submit(base.clone(), interest(1), RequestPriority::Visible, NOW)
        .expect("supported request queues");

    let mut unsupported = base.clone();
    unsupported.data_class = DataClass::Depth;
    unsupported.resolution = "book".to_string();
    assert_eq!(
        scheduler.submit(unsupported, interest(2), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::UnsupportedDataClass)
    );
    let mut resolution = base.clone();
    resolution.resolution = "1s".to_string();
    assert_eq!(
        scheduler.submit(resolution, interest(3), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::UnsupportedResolution)
    );
    let mut oversized = base.clone();
    oversized.maximum_items = nonzero_usize(17);
    assert_eq!(
        scheduler.submit(oversized, interest(4), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::PageLimitExceeded)
    );
    let mut cursor = base.clone();
    cursor.continuation = Some(Continuation::Cursor("unexpected".to_string()));
    assert_eq!(
        scheduler.submit(cursor, interest(5), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::InvalidContinuation)
    );

    let too_old = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: -1,
            end_unix_nanos: 100,
        },
    );
    assert_eq!(
        scheduler.submit(too_old, interest(6), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::LookbackExceeded)
    );
    let too_wide = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 8_000,
            end_unix_nanos: 9_500,
        },
    );
    assert_eq!(
        scheduler.submit(too_wide, interest(7), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::RequestSpanExceeded)
    );

    let mut end_time = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_500,
            end_unix_nanos: 9_900,
        },
    );
    end_time.continuation = Some(Continuation::EndBeforeUnixNanos(9_900));
    let mut end_scheduler = make_scheduler(PaginationStyle::EndTime, 2, 2, 8, 0);
    assert_eq!(
        end_scheduler.submit(end_time, interest(8), RequestPriority::Visible, NOW),
        Err(ProviderHistoryError::InvalidContinuation)
    );
    let still_valid = request(
        "eth-usd",
        HistoryRange {
            start_unix_nanos: 9_600,
            end_unix_nanos: 9_700,
        },
    );
    scheduler
        .submit(
            still_valid.clone(),
            interest(9),
            RequestPriority::Background,
            NOW,
        )
        .expect("later-expiring request queues");
    let outcome = scheduler
        .dispatch_next(19_501, MONOTONIC_NOW)
        .expect("expired work does not block dispatch");
    assert_eq!(outcome.expired.len(), 1);
    assert_eq!(outcome.expired[0].request, base);
    assert_eq!(outcome.expired[0].interests, vec![interest(1)]);
    assert_eq!(
        outcome
            .dispatch
            .expect("valid work still dispatches")
            .request,
        still_valid
    );
    assert_eq!(scheduler.inflight_len(), 1);
}

#[test]
fn visible_requests_rotate_fairly_across_instrument_lanes() {
    let mut scheduler = make_scheduler(PaginationStyle::None, 10, 8, 8, 0);
    let ranges = [
        HistoryRange {
            start_unix_nanos: 9_100,
            end_unix_nanos: 9_200,
        },
        HistoryRange {
            start_unix_nanos: 9_200,
            end_unix_nanos: 9_300,
        },
    ];
    for (request, interest_id) in [
        (request("btc-usd", ranges[0]), 1),
        (request("btc-usd", ranges[1]), 2),
        (request("eth-usd", ranges[0]), 3),
        (request("sol-usd", ranges[0]), 4),
    ] {
        scheduler
            .submit(
                request,
                interest(interest_id),
                RequestPriority::Visible,
                NOW,
            )
            .expect("visible lane request queues");
    }
    let dispatched = (0..4)
        .map(|offset| {
            scheduler
                .dispatch_next(NOW, MONOTONIC_NOW + offset)
                .expect("fair dispatch succeeds")
                .dispatch
                .expect("request dispatches")
                .request
                .instrument_id
        })
        .collect::<Vec<_>>();
    assert_eq!(dispatched, ["btc-usd", "eth-usd", "sol-usd", "btc-usd"]);
}

#[test]
fn exact_requests_deduplicate_while_visible_work_dispatches_first() {
    let mut scheduler = make_scheduler(PaginationStyle::None, 8, 8, 8, 0);
    let background = request(
        "eth-usd",
        HistoryRange {
            start_unix_nanos: 9_000,
            end_unix_nanos: 9_100,
        },
    );
    let visible = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_100,
            end_unix_nanos: 9_200,
        },
    );
    scheduler
        .submit(background, interest(1), RequestPriority::Background, NOW)
        .expect("background queues");
    scheduler
        .submit(
            visible.clone(),
            interest(2),
            RequestPriority::AdjacentPrefetch,
            NOW,
        )
        .expect("candidate queues");
    let duplicate = scheduler
        .submit(visible.clone(), interest(3), RequestPriority::Visible, NOW)
        .expect("duplicate merges and upgrades");
    assert_eq!(duplicate.deduplicated, 1);
    assert_eq!(scheduler.queued_len(), 2);
    let dispatch = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("dispatch succeeds")
        .dispatch
        .expect("visible request is ready");
    assert_eq!(dispatch.request, visible);
    assert_eq!(dispatch.priority, RequestPriority::Visible);

    let inflight_duplicate = scheduler
        .submit(
            dispatch.request.clone(),
            interest(4),
            RequestPriority::Visible,
            NOW,
        )
        .expect("inflight work also deduplicates");
    assert_eq!(inflight_duplicate.deduplicated, 1);
    assert_eq!(scheduler.inflight_len(), 1);
}

#[test]
fn visible_submission_derives_bounded_adjacent_prefetch_and_is_atomic() {
    let visible = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_600,
            end_unix_nanos: 9_700,
        },
    );
    let mut scheduler = make_scheduler(PaginationStyle::None, 8, 8, 8, 2);
    let submission = scheduler
        .submit_visible_with_prefetch(&visible, interest(1), NOW)
        .expect("visible and adjacent windows queue");
    assert_eq!(submission.queued_new, 3);
    assert_eq!(submission.adjacent_prefetches, 2);
    let first = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("dispatch succeeds")
        .dispatch
        .expect("visible request dispatches");
    assert_eq!(first.request.range, visible.range);
    assert_eq!(first.priority, RequestPriority::Visible);

    let mut bounded = make_scheduler(PaginationStyle::None, 8, 8, 2, 2);
    assert_eq!(
        bounded.submit_visible_with_prefetch(&visible, interest(2), NOW),
        Err(ProviderHistoryError::QueueFull { maximum: 2 })
    );
    assert_eq!(bounded.queued_len(), 0);
}

#[test]
fn rate_windows_and_dataset_concurrency_gate_dispatch_deterministically() {
    let mut scheduler = make_scheduler(PaginationStyle::None, 1, 2, 8, 0);
    for (interest_id, instrument) in [(1, "btc-usd"), (2, "eth-usd")] {
        scheduler
            .submit(
                request(
                    instrument,
                    HistoryRange {
                        start_unix_nanos: 9_000,
                        end_unix_nanos: 9_100,
                    },
                ),
                interest(interest_id),
                RequestPriority::Visible,
                NOW,
            )
            .expect("request queues");
    }
    assert!(
        scheduler
            .dispatch_next(NOW, MONOTONIC_NOW)
            .expect("first dispatch")
            .dispatch
            .is_some()
    );

    let mut concurrency = make_scheduler(PaginationStyle::None, 8, 1, 8, 0);
    for (interest_id, instrument) in [(3, "sol-usd"), (4, "xrp-usd")] {
        concurrency
            .submit(
                request(
                    instrument,
                    HistoryRange {
                        start_unix_nanos: 9_200,
                        end_unix_nanos: 9_300,
                    },
                ),
                interest(interest_id),
                RequestPriority::Visible,
                NOW,
            )
            .expect("concurrency fixture queues");
    }
    let first = concurrency
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("first concurrent dispatch succeeds")
        .dispatch
        .expect("first concurrent request is ready");
    assert_eq!(
        concurrency
            .dispatch_next(NOW, MONOTONIC_NOW)
            .expect("concurrency gate")
            .dispatch,
        None
    );
    concurrency
        .complete(
            first.dispatch_id,
            HistoryPage {
                request: first.request,
                items: vec![],
                next: None,
            },
            NOW,
        )
        .expect("completion releases concurrency");
    assert!(
        concurrency
            .dispatch_next(NOW, MONOTONIC_NOW)
            .expect("slot reopens")
            .dispatch
            .is_some()
    );
    assert_eq!(
        scheduler
            .dispatch_next(NOW, MONOTONIC_NOW)
            .expect("rate gate is explicit")
            .dispatch,
        None
    );
    assert_eq!(
        scheduler
            .dispatch_next(NOW + 100, MONOTONIC_NOW + 50)
            .expect("wall-clock movement cannot reset the rate window")
            .dispatch,
        None
    );
    assert!(
        scheduler
            .dispatch_next(NOW, MONOTONIC_NOW + 100)
            .expect("monotonic next window dispatch")
            .dispatch
            .is_some()
    );
}

#[test]
fn cancellation_preserves_shared_work_and_aborts_only_unobserved_inflight() {
    let mut scheduler = make_scheduler(PaginationStyle::None, 8, 1, 8, 0);
    let shared_request = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_000,
            end_unix_nanos: 9_100,
        },
    );
    scheduler
        .submit(
            shared_request.clone(),
            interest(1),
            RequestPriority::Visible,
            NOW,
        )
        .expect("first interest queues");
    scheduler
        .submit(shared_request, interest(2), RequestPriority::Visible, NOW)
        .expect("second interest deduplicates");
    let dispatch = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("dispatch succeeds")
        .dispatch
        .expect("work dispatches");
    assert_eq!(
        scheduler.cancel(interest(1)),
        CancelOutcome {
            queued_removed: 0,
            provider_aborts: vec![],
        }
    );
    assert_eq!(scheduler.inflight_len(), 1);
    assert_eq!(
        scheduler.cancel(interest(2)),
        CancelOutcome {
            queued_removed: 0,
            provider_aborts: vec![dispatch.dispatch_id],
        }
    );
    assert_eq!(scheduler.inflight_len(), 1);

    let queued = request(
        "eth-usd",
        HistoryRange {
            start_unix_nanos: 9_200,
            end_unix_nanos: 9_300,
        },
    );
    scheduler
        .submit(queued, interest(3), RequestPriority::Background, NOW)
        .expect("queued cancellation fixture submits");
    assert_eq!(
        scheduler
            .dispatch_next(NOW, MONOTONIC_NOW)
            .expect("aborting slot remains occupied")
            .dispatch,
        None
    );
    let expired = scheduler
        .dispatch_next(19_201, MONOTONIC_NOW)
        .expect("expiry sweep runs while the slot remains occupied");
    assert!(expired.dispatch.is_none());
    assert_eq!(expired.expired.len(), 1);
    assert_eq!(expired.expired[0].interests, vec![interest(3)]);
    scheduler
        .acknowledge_abort(dispatch.dispatch_id)
        .expect("provider abort releases the slot");
    assert_eq!(scheduler.inflight_len(), 0);
    let cancellable = request(
        "ada-usd",
        HistoryRange {
            start_unix_nanos: 9_400,
            end_unix_nanos: 9_500,
        },
    );
    scheduler
        .submit(cancellable, interest(4), RequestPriority::Background, NOW)
        .expect("queued cancellation fixture submits");
    assert_eq!(
        scheduler.cancel(interest(4)),
        CancelOutcome {
            queued_removed: 1,
            provider_aborts: vec![],
        }
    );
    assert_eq!(scheduler.queued_len(), 0);
}

#[test]
fn fetch_failures_retry_within_bounds_then_release_consumers() {
    let mut scheduler = make_scheduler(PaginationStyle::None, 8, 1, 8, 0);
    let failed_request = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_000,
            end_unix_nanos: 9_100,
        },
    );
    scheduler
        .submit(
            failed_request.clone(),
            interest(1),
            RequestPriority::Visible,
            NOW,
        )
        .expect("failed request queues");
    let first = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("first attempt dispatches")
        .dispatch
        .expect("first attempt is ready");
    assert_eq!(
        scheduler
            .fail_fetch(first.dispatch_id)
            .expect("first failure finalizes"),
        FetchFailureOutcome::Requeued { attempts: 1 }
    );
    assert_eq!(scheduler.inflight_len(), 0);
    assert_eq!(scheduler.queued_len(), 1);

    let second = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW + 1)
        .expect("retry dispatches")
        .dispatch
        .expect("retry is ready");
    assert_eq!(second.request, failed_request);
    assert_eq!(
        scheduler
            .fail_fetch(second.dispatch_id)
            .expect("terminal failure finalizes"),
        FetchFailureOutcome::Terminal {
            attempts: 2,
            interests: vec![interest(1)],
        }
    );
    assert_eq!(scheduler.inflight_len(), 0);
    assert_eq!(scheduler.queued_len(), 0);
    scheduler
        .submit(failed_request, interest(1), RequestPriority::Visible, NOW)
        .expect("terminal failure releases the interest identity");
}

#[test]
fn validated_pages_schedule_progressing_continuations_without_losing_retry_state() {
    let mut scheduler = make_scheduler(PaginationStyle::OpaqueCursor, 8, 8, 8, 0);
    let mut initial = request(
        "btc-usd",
        HistoryRange {
            start_unix_nanos: 9_000,
            end_unix_nanos: 9_100,
        },
    );
    initial.continuation = Some(Continuation::Cursor("page-1".to_string()));
    scheduler
        .submit(initial.clone(), interest(1), RequestPriority::Visible, NOW)
        .expect("request queues");
    let dispatch = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("dispatch succeeds")
        .dispatch
        .expect("request dispatches");
    let mut mismatch = initial.clone();
    mismatch.instrument_id = "eth-usd".to_string();
    assert_eq!(
        scheduler.complete(
            dispatch.dispatch_id,
            HistoryPage {
                request: mismatch,
                items: vec![],
                next: None,
            },
            NOW,
        ),
        Err(ProviderHistoryError::CompletionMismatch)
    );
    assert_eq!(scheduler.inflight_len(), 1);

    assert_eq!(
        scheduler.complete(
            dispatch.dispatch_id,
            HistoryPage {
                request: initial.clone(),
                items: vec![item(1, 8_999)],
                next: None,
            },
            NOW,
        ),
        Err(ProviderHistoryError::InvalidPage(
            "item falls outside requested range"
        ))
    );
    assert_eq!(scheduler.inflight_len(), 1);

    let completion = scheduler
        .complete(
            dispatch.dispatch_id,
            HistoryPage {
                request: initial,
                items: vec![item(1, 9_010), item(2, 9_020)],
                next: Some(Continuation::Cursor("page-2".to_string())),
            },
            NOW + 10_000,
        )
        .expect("valid page completes");
    assert!(completion.continuation_scheduled());
    let next = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("continuation dispatch succeeds")
        .dispatch
        .expect("continuation is queued");
    assert_eq!(
        next.request.continuation,
        Some(Continuation::Cursor("page-2".to_string()))
    );

    let second_page = HistoryPage {
        request: next.request.clone(),
        items: vec![item(3, 9_030)],
        next: Some(Continuation::Cursor("page-3".to_string())),
    };
    scheduler
        .complete(next.dispatch_id, second_page, NOW + 20_000)
        .expect("a new cursor progresses");
    let third = scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("third dispatch succeeds")
        .dispatch
        .expect("third page is queued");
    assert_eq!(
        scheduler.complete(
            third.dispatch_id,
            HistoryPage {
                request: third.request,
                items: vec![item(4, 9_040)],
                next: Some(Continuation::Cursor("page-1".to_string())),
            },
            NOW + 30_000,
        ),
        Err(ProviderHistoryError::InvalidContinuation)
    );
    assert_eq!(scheduler.inflight_len(), 1);
}

#[test]
fn end_time_pages_reject_items_newer_than_the_continuation_cutoff() {
    let mut end_time_scheduler = make_scheduler(PaginationStyle::EndTime, 8, 8, 8, 0);
    let mut end_time_request = request(
        "eth-usd",
        HistoryRange {
            start_unix_nanos: 9_000,
            end_unix_nanos: 9_900,
        },
    );
    end_time_request.continuation = Some(Continuation::EndBeforeUnixNanos(9_500));
    end_time_scheduler
        .submit(
            end_time_request.clone(),
            interest(2),
            RequestPriority::Visible,
            NOW,
        )
        .expect("end-time page queues");
    let end_time_dispatch = end_time_scheduler
        .dispatch_next(NOW, MONOTONIC_NOW)
        .expect("end-time dispatch succeeds")
        .dispatch
        .expect("end-time request dispatches");
    assert_eq!(
        end_time_scheduler.complete(
            end_time_dispatch.dispatch_id,
            HistoryPage {
                request: end_time_request,
                items: vec![item(1, 9_600)],
                next: None,
            },
            NOW,
        ),
        Err(ProviderHistoryError::InvalidPage(
            "item falls outside requested range"
        ))
    );
}
