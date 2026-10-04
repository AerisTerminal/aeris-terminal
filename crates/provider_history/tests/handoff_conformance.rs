use aeris_provider_history::{
    HandoffCoordinator, HandoffState, LiveAcceptance, ProviderHistoryError, SequencedHistory,
    VerifiedHistorySnapshot,
};
use std::num::{NonZeroU64, NonZeroUsize};

#[test]
fn snapshot_cutover_discards_overlap_and_releases_only_contiguous_live_data() {
    let mut coordinator = HandoffCoordinator::new(nonzero_usize(4));
    assert_eq!(
        coordinator
            .push_live(sequenced(2, "overlap"))
            .expect("first live item buffers"),
        LiveAcceptance::Buffered
    );
    assert_eq!(
        coordinator
            .push_live(sequenced(2, "duplicate"))
            .expect("duplicate is harmless"),
        LiveAcceptance::Duplicate
    );
    coordinator
        .push_live(sequenced(4, "live-4"))
        .expect("snapshot may fill a buffered prefix gap");
    let snapshot = VerifiedHistorySnapshot::try_new(
        nonzero_u64(1),
        vec![
            sequenced(1, "history-1"),
            sequenced(2, "history-2"),
            sequenced(3, "history-3"),
        ],
    )
    .expect("snapshot is contiguous");
    let batch = coordinator
        .install_snapshot(snapshot)
        .expect("cutover succeeds");
    assert_eq!(batch.live, vec![sequenced(4, "live-4")]);
    assert_eq!(
        coordinator.state(),
        HandoffState::Live {
            generation: 1,
            last_sequence: 4,
        }
    );
    assert_eq!(
        coordinator
            .push_live(sequenced(5, "live-5"))
            .expect("next live item is accepted"),
        LiveAcceptance::Accepted(sequenced(5, "live-5"))
    );
    assert_eq!(
        coordinator.push_live(sequenced(7, "gap")),
        Err(ProviderHistoryError::SequenceGap {
            expected: 6,
            actual: 7,
        })
    );
    assert_eq!(
        coordinator.state(),
        HandoffState::SnapshotRequired {
            minimum_generation: 1,
            minimum_watermark: 7,
        }
    );
    assert_eq!(
        coordinator.install_snapshot(
            VerifiedHistorySnapshot::try_new(nonzero_u64(1), vec![sequenced(1, "stale")])
                .expect("stale fixture is contiguous"),
        ),
        Err(ProviderHistoryError::InvalidPage(
            "snapshot generation did not advance"
        ))
    );
    assert_eq!(
        coordinator.install_snapshot(
            VerifiedHistorySnapshot::try_new(
                nonzero_u64(2),
                (1..=5)
                    .map(|sequence| sequenced(sequence, "incomplete"))
                    .collect(),
            )
            .expect("incomplete recovery fixture is contiguous"),
        ),
        Err(ProviderHistoryError::InvalidPage(
            "snapshot sequence watermark regressed"
        ))
    );
}

#[test]
fn live_buffer_overflow_and_snapshot_gaps_fail_closed() {
    let mut coordinator = HandoffCoordinator::new(nonzero_usize(1));
    coordinator
        .push_live(sequenced(5, "live-5"))
        .expect("first item buffers");
    assert_eq!(
        coordinator.push_live(sequenced(6, "overflow")),
        Err(ProviderHistoryError::LiveBufferFull { maximum: 1 })
    );
    assert_eq!(
        coordinator.state(),
        HandoffState::SnapshotRequired {
            minimum_generation: 0,
            minimum_watermark: 6,
        }
    );
    assert_eq!(
        coordinator.install_snapshot(
            VerifiedHistorySnapshot::try_new(
                nonzero_u64(1),
                (1..=4)
                    .map(|sequence| sequenced(sequence, "history"))
                    .collect(),
            )
            .expect("overflow recovery fixture is contiguous"),
        ),
        Err(ProviderHistoryError::InvalidPage(
            "snapshot sequence watermark regressed"
        ))
    );
    assert_eq!(
        VerifiedHistorySnapshot::try_new(
            nonzero_u64(1),
            vec![sequenced(1, "one"), sequenced(3, "three")],
        ),
        Err(ProviderHistoryError::SequenceGap {
            expected: 2,
            actual: 3,
        })
    );
    assert_eq!(
        VerifiedHistorySnapshot::try_new(
            nonzero_u64(1),
            vec![
                sequenced(u64::MAX, "maximum"),
                sequenced(u64::MAX, "duplicate"),
            ],
        ),
        Err(ProviderHistoryError::InvalidPage(
            "snapshot sequence cannot advance beyond the maximum"
        ))
    );

    let mut retry = HandoffCoordinator::new(nonzero_usize(2));
    retry
        .push_live(sequenced(5, "live-5"))
        .expect("live item buffers");
    assert_eq!(
        retry.install_snapshot(
            VerifiedHistorySnapshot::try_new(
                nonzero_u64(1),
                (1..=3)
                    .map(|sequence| sequenced(sequence, "history"))
                    .collect(),
            )
            .expect("first retry fixture is contiguous"),
        ),
        Err(ProviderHistoryError::SequenceGap {
            expected: 4,
            actual: 5,
        })
    );
    let recovered = retry
        .install_snapshot(
            VerifiedHistorySnapshot::try_new(
                nonzero_u64(2),
                (1..=4)
                    .map(|sequence| sequenced(sequence, "history"))
                    .collect(),
            )
            .expect("replacement snapshot is contiguous"),
        )
        .expect("replacement snapshot retains buffered live data");
    assert_eq!(recovered.live, vec![sequenced(5, "live-5")]);
}

#[test]
fn empty_snapshot_uses_an_explicit_global_sequence_watermark() {
    assert_eq!(
        VerifiedHistorySnapshot::<&str>::try_new(nonzero_u64(1), vec![]),
        Err(ProviderHistoryError::InvalidPage(
            "empty snapshot requires an explicit cutover watermark"
        ))
    );

    let mut coordinator = HandoffCoordinator::new(nonzero_usize(2));
    coordinator
        .push_live(sequenced(42, "live-42"))
        .expect("global live sequence buffers");
    let batch = coordinator
        .install_snapshot(VerifiedHistorySnapshot::empty_with_watermark(
            nonzero_u64(1),
            41,
        ))
        .expect("explicit empty watermark permits contiguous cutover");
    assert_eq!(batch.snapshot.items(), []);
    assert_eq!(batch.live, vec![sequenced(42, "live-42")]);
    assert_eq!(
        coordinator
            .push_live(sequenced(43, "live-43"))
            .expect("global sequence continues after cutover"),
        LiveAcceptance::Accepted(sequenced(43, "live-43"))
    );
    assert_eq!(
        coordinator.install_snapshot(
            VerifiedHistorySnapshot::try_new(nonzero_u64(1), vec![sequenced(43, "stale")])
                .expect("stale snapshot is internally contiguous"),
        ),
        Err(ProviderHistoryError::InvalidPage(
            "snapshot generation did not advance"
        ))
    );
    assert_eq!(
        coordinator.state(),
        HandoffState::SnapshotRequired {
            minimum_generation: 1,
            minimum_watermark: 43,
        }
    );
    assert_eq!(
        coordinator.push_live(sequenced(44, "must-wait")),
        Err(ProviderHistoryError::SnapshotRequired)
    );
}

fn nonzero_u64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test value must be non-zero")
}

fn nonzero_usize(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test value must be non-zero")
}

fn sequenced(sequence: u64, value: &'static str) -> SequencedHistory<&'static str> {
    SequencedHistory {
        sequence: nonzero_u64(sequence),
        value,
    }
}
