use crate::ProviderHistoryError;
use std::{
    collections::VecDeque,
    num::{NonZeroU64, NonZeroUsize},
};

/// One fully owned value carrying the provider/canonical cutover sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequencedHistory<T> {
    pub sequence: NonZeroU64,
    pub value: T,
}

/// Validated contiguous backfill snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedHistorySnapshot<T> {
    generation: NonZeroU64,
    items: Vec<SequencedHistory<T>>,
    watermark: u64,
}

impl<T> VerifiedHistorySnapshot<T> {
    /// Validates contiguity and derives the live cutover watermark.
    ///
    /// # Errors
    ///
    /// Returns an error when snapshot sequences contain a gap.
    pub fn try_new(
        generation: NonZeroU64,
        items: Vec<SequencedHistory<T>>,
    ) -> Result<Self, ProviderHistoryError> {
        if items.is_empty() {
            return Err(ProviderHistoryError::InvalidPage(
                "empty snapshot requires an explicit cutover watermark",
            ));
        }
        validate_contiguous(&items)?;
        let watermark = items.last().map(|item| item.sequence.get()).ok_or(
            ProviderHistoryError::InvalidPage(
                "empty snapshot requires an explicit cutover watermark",
            ),
        )?;
        Ok(Self {
            generation,
            items,
            watermark,
        })
    }

    /// Creates an empty snapshot at a provider-supplied live cutover watermark.
    #[must_use]
    pub fn empty_with_watermark(generation: NonZeroU64, watermark: u64) -> Self {
        Self {
            generation,
            items: Vec::new(),
            watermark,
        }
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation.get()
    }

    #[must_use]
    pub const fn watermark(&self) -> u64 {
        self.watermark
    }

    #[must_use]
    pub fn items(&self) -> &[SequencedHistory<T>] {
        &self.items
    }

    #[must_use]
    pub fn into_items(self) -> Vec<SequencedHistory<T>> {
        self.items
    }
}

/// Snapshot plus exactly contiguous live values accepted at cutover.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffBatch<T> {
    pub snapshot: VerifiedHistorySnapshot<T>,
    pub live: Vec<SequencedHistory<T>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffState {
    AwaitingSnapshot,
    Live {
        generation: u64,
        last_sequence: u64,
    },
    SnapshotRequired {
        minimum_generation: u64,
        minimum_watermark: u64,
    },
}

/// Outcome of a live item before or after snapshot installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveAcceptance<T> {
    Buffered,
    Accepted(SequencedHistory<T>),
    Duplicate,
}

/// Bounded state machine joining history and live delivery without gaps or duplicates.
#[derive(Clone)]
pub struct HandoffCoordinator<T> {
    maximum_buffered_live: NonZeroUsize,
    buffered_live: VecDeque<SequencedHistory<T>>,
    state: HandoffState,
}

impl<T> HandoffCoordinator<T> {
    #[must_use]
    pub fn new(maximum_buffered_live: NonZeroUsize) -> Self {
        Self {
            maximum_buffered_live,
            buffered_live: VecDeque::new(),
            state: HandoffState::AwaitingSnapshot,
        }
    }

    #[must_use]
    pub const fn state(&self) -> HandoffState {
        self.state
    }

    /// Discards buffered live values and requires a snapshot covering an
    /// observed sequence that could not be retained by a downstream bound.
    pub fn require_snapshot(&mut self, observed_sequence: u64) {
        let (minimum_generation, state_watermark) = match self.state {
            HandoffState::AwaitingSnapshot => (0, 0),
            HandoffState::Live {
                generation,
                last_sequence,
            } => (generation, last_sequence),
            HandoffState::SnapshotRequired {
                minimum_generation,
                minimum_watermark,
            } => (minimum_generation, minimum_watermark),
        };
        let buffered_watermark = self
            .buffered_live
            .back()
            .map_or(0, |item| item.sequence.get());
        self.buffered_live.clear();
        self.state = HandoffState::SnapshotRequired {
            minimum_generation,
            minimum_watermark: observed_sequence
                .max(state_watermark)
                .max(buffered_watermark),
        };
    }

    /// Requires a snapshot newer than an already-published generation and at
    /// least as complete as its watermark.
    pub fn require_snapshot_after(&mut self, generation: u64, watermark: u64) {
        self.require_snapshot(watermark);
        if let HandoffState::SnapshotRequired {
            minimum_generation,
            minimum_watermark,
        } = &mut self.state
        {
            *minimum_generation = (*minimum_generation).max(generation);
            *minimum_watermark = (*minimum_watermark).max(watermark);
        }
    }

    /// Buffers live data before history arrives, then enforces contiguous delivery.
    ///
    /// # Errors
    ///
    /// Returns an error and requires a new snapshot on overflow or a sequence gap.
    pub fn push_live(
        &mut self,
        item: SequencedHistory<T>,
    ) -> Result<LiveAcceptance<T>, ProviderHistoryError> {
        match self.state {
            HandoffState::AwaitingSnapshot => {
                if let Some(previous) = self.buffered_live.back() {
                    let previous = previous.sequence.get();
                    if item.sequence.get() <= previous {
                        return Ok(LiveAcceptance::Duplicate);
                    }
                }
                if self.buffered_live.len() >= self.maximum_buffered_live.get() {
                    let minimum_watermark = self
                        .buffered_live
                        .back()
                        .map_or(item.sequence.get(), |buffered| {
                            buffered.sequence.get().max(item.sequence.get())
                        });
                    self.state = HandoffState::SnapshotRequired {
                        minimum_generation: 0,
                        minimum_watermark,
                    };
                    self.buffered_live.clear();
                    return Err(ProviderHistoryError::LiveBufferFull {
                        maximum: self.maximum_buffered_live.get(),
                    });
                }
                self.buffered_live.push_back(item);
                Ok(LiveAcceptance::Buffered)
            }
            HandoffState::Live {
                generation,
                last_sequence,
            } => {
                if item.sequence.get() <= last_sequence {
                    return Ok(LiveAcceptance::Duplicate);
                }
                let expected = last_sequence.saturating_add(1);
                if item.sequence.get() != expected {
                    self.state = HandoffState::SnapshotRequired {
                        minimum_generation: generation,
                        minimum_watermark: item.sequence.get(),
                    };
                    return Err(ProviderHistoryError::SequenceGap {
                        expected,
                        actual: item.sequence.get(),
                    });
                }
                self.state = HandoffState::Live {
                    generation,
                    last_sequence: item.sequence.get(),
                };
                Ok(LiveAcceptance::Accepted(item))
            }
            HandoffState::SnapshotRequired { .. } => Err(ProviderHistoryError::SnapshotRequired),
        }
    }

    /// Installs a backfill snapshot and releases only its contiguous live suffix.
    ///
    /// # Errors
    ///
    /// Returns an error and latches snapshot-required if buffered live data has a gap.
    pub fn install_snapshot(
        &mut self,
        snapshot: VerifiedHistorySnapshot<T>,
    ) -> Result<HandoffBatch<T>, ProviderHistoryError> {
        let recovery_floor = match self.state {
            HandoffState::Live {
                generation,
                last_sequence,
            } => Some((generation, last_sequence)),
            HandoffState::SnapshotRequired {
                minimum_generation,
                minimum_watermark,
            } => Some((minimum_generation, minimum_watermark)),
            HandoffState::AwaitingSnapshot => None,
        };
        if let Some((minimum_generation, minimum_watermark)) = recovery_floor {
            if snapshot.generation() <= minimum_generation {
                self.state = HandoffState::SnapshotRequired {
                    minimum_generation,
                    minimum_watermark,
                };
                return Err(ProviderHistoryError::InvalidPage(
                    "snapshot generation did not advance",
                ));
            }
            if snapshot.watermark() < minimum_watermark {
                self.state = HandoffState::SnapshotRequired {
                    minimum_generation,
                    minimum_watermark,
                };
                return Err(ProviderHistoryError::InvalidPage(
                    "snapshot sequence watermark regressed",
                ));
            }
        }
        if let Some((expected, actual)) =
            buffered_suffix_gap(&self.buffered_live, snapshot.watermark())
        {
            self.state = HandoffState::SnapshotRequired {
                minimum_generation: snapshot.generation(),
                minimum_watermark: snapshot.watermark(),
            };
            return Err(ProviderHistoryError::SequenceGap { expected, actual });
        }
        let mut live = Vec::new();
        while let Some(item) = self.buffered_live.pop_front() {
            if item.sequence.get() > snapshot.watermark() {
                live.push(item);
            }
        }
        let last_sequence = live
            .last()
            .map_or(snapshot.watermark(), |item| item.sequence.get());
        self.state = HandoffState::Live {
            generation: snapshot.generation(),
            last_sequence,
        };
        Ok(HandoffBatch { snapshot, live })
    }
}

fn buffered_suffix_gap<T>(
    items: &VecDeque<SequencedHistory<T>>,
    watermark: u64,
) -> Option<(u64, u64)> {
    let mut expected = watermark.saturating_add(1);
    for item in items.iter().filter(|item| item.sequence.get() > watermark) {
        let actual = item.sequence.get();
        if actual != expected {
            return Some((expected, actual));
        }
        expected = actual.saturating_add(1);
    }
    None
}

fn validate_contiguous<T>(items: &[SequencedHistory<T>]) -> Result<(), ProviderHistoryError> {
    for pair in items.windows(2) {
        let Some(expected) = pair[0].sequence.get().checked_add(1) else {
            return Err(ProviderHistoryError::InvalidPage(
                "snapshot sequence cannot advance beyond the maximum",
            ));
        };
        let actual = pair[1].sequence.get();
        if actual != expected {
            return Err(ProviderHistoryError::SequenceGap { expected, actual });
        }
    }
    Ok(())
}
