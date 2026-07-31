//! Transport- and provider-independent application use-case contracts.

use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
    InstrumentValidationError,
};
use axiusflow_market_data::{BarDefinition, MarketBar, MarketDataValidationError};
use axiusflow_protocols::{
    MAX_STREAM_SNAPSHOT_ITEMS, MarketSnapshotIdentityRef, MarketValueChecksumRef, SequenceTracker,
    StreamProtocolError, StreamSnapshot, compute_market_snapshot_checksum,
};
pub use axiusflow_protocols::{
    MarketEventProvenance, Provenanced, SequenceDecision, SnapshotEvidence, StreamDelta,
};
use core::fmt;
use std::{error::Error, num::NonZeroU64, sync::Arc};

/// Maximum bars accepted by the embedded replay boundary.
pub const MAX_EMBEDDED_REPLAY_BARS: usize = MAX_STREAM_SNAPSHOT_ITEMS;

const EMBEDDED_REPLAY_START_SECONDS: i64 = 1_735_689_600;
const EMBEDDED_REPLAY_INTERVAL_SECONDS: i64 = 60;

/// Executes one application command without exposing transport details.
pub trait UseCase<Command> {
    type Output;
    type Error;

    /// Executes the command according to the use case's business rules.
    ///
    /// # Errors
    ///
    /// Returns the implementation's error when validation or execution fails.
    fn execute(&self, command: Command) -> Result<Self::Output, Self::Error>;
}

/// Request metadata propagated through application boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestContext {
    pub actor_id: String,
    pub correlation_id: String,
    pub causation_id: Option<String>,
}

/// Provenance for replay data exposed to application consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayProvenance {
    /// Deterministic local data used before a real stream transport is connected.
    EmbeddedFixture,
}

impl ReplayProvenance {
    /// Returns a user-facing source label that does not imply a live connection.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::EmbeddedFixture => "embedded replay fixture",
        }
    }
}

/// One fixed-point bar inseparable from source, ownership, quality, and entitlement evidence.
pub type ProvenancedMarketBar = Provenanced<MarketBar>;

/// Revalidates a market value and its canonical evidence at an application boundary.
///
/// # Errors
///
/// Returns an error when the value is invalid or its evidence is incomplete or mismatched.
pub fn validate_provenanced_market_bar(
    item: &ProvenancedMarketBar,
) -> Result<(), ReplayValidationError> {
    let bar = item.value();
    let provenance = item.provenance();
    bar.validate()?;
    if provenance.source_sequence != bar.source_sequence {
        return Err(ReplayValidationError::ProvenanceSequenceMismatch {
            bar: bar.source_sequence,
            provenance: provenance.source_sequence,
        });
    }
    let expected_exchange_timestamp_unix_nanos = bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or(ReplayValidationError::ExchangeTimestampOverflow {
            source_sequence: bar.source_sequence,
        })?;
    if provenance.exchange_timestamp_unix_nanos != expected_exchange_timestamp_unix_nanos {
        return Err(ReplayValidationError::ProvenanceExchangeTimestampMismatch {
            source_sequence: bar.source_sequence,
            bar_seconds: bar.exchange_timestamp_seconds,
            provenance_nanos: provenance.exchange_timestamp_unix_nanos,
        });
    }
    for (field, value) in [
        ("event_id", provenance.event_id.as_str()),
        ("producer", provenance.producer.as_str()),
        ("source_id", provenance.source_id.as_str()),
        (
            "entitlement_revision",
            provenance.entitlement_revision.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            return Err(ReplayValidationError::MissingProvenance(field));
        }
    }
    if provenance.ownership_epoch == 0 || provenance.schema_version == 0 {
        return Err(ReplayValidationError::InvalidProvenanceRevision);
    }
    Ok(())
}

fn try_provenanced_market_bar(
    bar: MarketBar,
    provenance: MarketEventProvenance,
) -> Result<ProvenancedMarketBar, ReplayValidationError> {
    let item = Provenanced::new(bar, provenance);
    validate_provenanced_market_bar(&item)?;
    Ok(item)
}

fn snapshot_checksum(
    evidence: &SnapshotEvidence,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    bars: &[ProvenancedMarketBar],
) -> [u8; 32] {
    compute_market_snapshot_checksum(
        evidence,
        MarketSnapshotIdentityRef {
            instrument_id: instrument.instrument_id.as_str(),
            instrument_revision: instrument.revision,
            bar_definition_id: &bar_definition.definition_id,
            bar_definition_version: bar_definition.version,
            bar_interval_seconds: bar_definition.interval_seconds,
        },
        bars.iter().map(|item| {
            let bar = item.value();
            MarketValueChecksumRef {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                provenance: item.provenance(),
            }
        }),
    )
}

/// A validated, bounded replay snapshot tied to one instrument revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplaySnapshot {
    instrument: InstrumentRevision,
    provenance: ReplayProvenance,
    bar_definition: BarDefinition,
    stream: StreamSnapshot<ProvenancedMarketBar>,
    evidence: SnapshotEvidence,
}

impl ReplaySnapshot {
    /// Validates an immutable embedded replay snapshot before it crosses into a UI adapter.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid reference or bar-definition data, invalid
    /// OHLCV values, sequence gaps, non-increasing timestamps, or stream bounds.
    pub fn try_new(
        instrument: InstrumentRevision,
        provenance: ReplayProvenance,
        bar_definition: BarDefinition,
        bars: Vec<MarketBar>,
    ) -> Result<Self, ReplayValidationError> {
        let bars = bars
            .into_iter()
            .map(|bar| {
                let evidence = embedded_event_provenance(&bar);
                try_provenanced_market_bar(bar, evidence)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let first = bars.first().ok_or(StreamProtocolError::EmptySnapshot)?;
        let last = bars.last().ok_or(StreamProtocolError::EmptySnapshot)?;
        let mut evidence = SnapshotEvidence {
            partition_id: first.provenance().partition_id,
            ownership_epoch: first.provenance().ownership_epoch,
            generation: 1,
            first_sequence: first.value().source_sequence,
            last_sequence: last.value().source_sequence,
            schema_version: first.provenance().schema_version,
            checksum: [0; 32],
        };
        evidence.checksum = snapshot_checksum(&evidence, &instrument, &bar_definition, &bars);
        Self::try_new_provenanced(instrument, provenance, bar_definition, evidence, bars)
    }

    /// Validates a provenance-bearing immutable snapshot from replay or live transport.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid reference, evidence, sequence, timestamp, checksum, or bounds.
    pub fn try_new_provenanced(
        instrument: InstrumentRevision,
        provenance: ReplayProvenance,
        bar_definition: BarDefinition,
        evidence: SnapshotEvidence,
        bars: Vec<ProvenancedMarketBar>,
    ) -> Result<Self, ReplayValidationError> {
        instrument.validate()?;
        bar_definition.validate()?;
        if evidence.ownership_epoch == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence(
                "ownership_epoch",
            ));
        }
        if evidence.generation == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence("generation"));
        }
        if evidence.schema_version == 0 {
            return Err(ReplayValidationError::InvalidSnapshotEvidence(
                "schema_version",
            ));
        }
        let Some(first_item) = bars.first() else {
            return Err(StreamProtocolError::EmptySnapshot.into());
        };

        validate_provenanced_market_bar(first_item)?;
        let first_bar = *first_item.value();
        let mut previous = first_bar;
        for item in &bars {
            validate_provenanced_market_bar(item)?;
            let item_provenance = item.provenance();
            if item_provenance.partition_id != evidence.partition_id {
                return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                    "partition_id",
                ));
            }
            if item_provenance.ownership_epoch != evidence.ownership_epoch {
                return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                    "ownership_epoch",
                ));
            }
            if item_provenance.schema_version != evidence.schema_version {
                return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                    "schema_version",
                ));
            }
        }
        for item in bars.iter().skip(1) {
            let bar = *item.value();
            StreamDelta::try_new(previous.source_sequence, bar.source_sequence, ())?;
            if bar.exchange_timestamp_seconds <= previous.exchange_timestamp_seconds {
                return Err(ReplayValidationError::NonIncreasingTimestamp {
                    source_sequence: bar.source_sequence,
                });
            }
            previous = bar;
        }
        if evidence.first_sequence != first_bar.source_sequence {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "first_sequence",
            ));
        }
        if evidence.last_sequence != previous.source_sequence {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "last_sequence",
            ));
        }
        if snapshot_checksum(&evidence, &instrument, &bar_definition, &bars) != evidence.checksum {
            return Err(ReplayValidationError::SnapshotChecksumMismatch);
        }

        let stream =
            StreamSnapshot::try_new(first_bar.source_sequence, previous.source_sequence, bars)?;
        Ok(Self {
            instrument,
            provenance,
            bar_definition,
            stream,
            evidence,
        })
    }

    /// Returns the immutable instrument revision associated with every bar.
    #[must_use]
    pub const fn instrument(&self) -> &InstrumentRevision {
        &self.instrument
    }

    /// Returns truthful transport provenance for display and diagnostics.
    #[must_use]
    pub const fn provenance(&self) -> ReplayProvenance {
        self.provenance
    }

    /// Returns the versioned definition used to build this bar series.
    #[must_use]
    pub const fn bar_definition(&self) -> &BarDefinition {
        &self.bar_definition
    }

    /// Returns the validated bounded stream snapshot.
    #[must_use]
    pub const fn stream(&self) -> &StreamSnapshot<ProvenancedMarketBar> {
        &self.stream
    }

    /// Returns the retained snapshot identity and computed integrity evidence.
    #[must_use]
    pub const fn evidence(&self) -> &SnapshotEvidence {
        &self.evidence
    }

    /// Reissues the same validated values under a nonzero snapshot generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation is zero or rebuilt evidence is invalid.
    pub fn try_with_generation(&self, generation: u64) -> Result<Self, ReplayValidationError> {
        let mut evidence = self.evidence.clone();
        evidence.generation = generation;
        evidence.checksum = snapshot_checksum(
            &evidence,
            &self.instrument,
            &self.bar_definition,
            self.bars(),
        );
        Self::try_new_provenanced(
            self.instrument.clone(),
            self.provenance,
            self.bar_definition.clone(),
            evidence,
            self.bars().to_vec(),
        )
    }

    /// Returns contiguous fixed-point bars with inseparable canonical evidence.
    #[must_use]
    pub fn bars(&self) -> &[ProvenancedMarketBar] {
        self.stream.items()
    }

    /// Returns the inclusive source-sequence range.
    #[must_use]
    pub const fn sequence_range(&self) -> (u64, u64) {
        (self.stream.first_sequence(), self.stream.last_sequence())
    }
}

/// One transport-neutral update delivered to replay consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayStreamUpdate {
    Snapshot(ReplaySnapshot),
    Delta(StreamDelta<ProvenancedMarketBar>),
}

/// Consumer-side replay state that never speculates across sequence gaps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplaySession {
    sequence_tracker: SequenceTracker,
    last_exchange_timestamp_seconds: i64,
}

impl ReplaySession {
    /// Installs an initial snapshot as the accepted replay baseline.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn try_new(snapshot: &ReplaySnapshot) -> Result<Self, ReplayValidationError> {
        let mut sequence_tracker = SequenceTracker::default();
        sequence_tracker.install_snapshot(snapshot.stream())?;
        let last_exchange_timestamp_seconds = snapshot
            .bars()
            .last()
            .ok_or(StreamProtocolError::EmptySnapshot)?
            .value()
            .exchange_timestamp_seconds;
        Ok(Self {
            sequence_tracker,
            last_exchange_timestamp_seconds,
        })
    }

    /// Replaces current stream state with a fresh validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot establish resumable sequence state.
    pub fn install_snapshot(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        *self = Self::try_new(snapshot)?;
        Ok(())
    }

    /// Validates and classifies one delta against the accepted replay state.
    ///
    /// Accepted deltas advance state. Duplicates do not mutate state. A gap
    /// permanently requires a fresh snapshot before later deltas can be accepted.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid market data, a non-increasing timestamp on
    /// the next contiguous delta, or sequence overflow.
    pub fn accept_delta(
        &mut self,
        delta: &StreamDelta<ProvenancedMarketBar>,
    ) -> Result<SequenceDecision, ReplayValidationError> {
        validate_provenanced_market_bar(delta.item())?;
        let mut candidate = self.sequence_tracker;
        let decision = candidate.accept_delta(delta)?;
        match decision {
            SequenceDecision::Accepted => {
                if delta.item().value().exchange_timestamp_seconds
                    <= self.last_exchange_timestamp_seconds
                {
                    return Err(ReplayValidationError::NonIncreasingTimestamp {
                        source_sequence: delta.sequence(),
                    });
                }
                self.sequence_tracker = candidate;
                self.last_exchange_timestamp_seconds =
                    delta.item().value().exchange_timestamp_seconds;
            }
            SequenceDecision::Gap { .. } | SequenceDecision::SnapshotRequired => {
                self.sequence_tracker = candidate;
            }
            SequenceDecision::Duplicate => {}
        }
        Ok(decision)
    }

    /// Returns the next source sequence expected by this session.
    #[must_use]
    pub const fn expected_sequence(self) -> Option<u64> {
        self.sequence_tracker.expected_sequence()
    }

    /// Returns whether a fresh snapshot is required before accepting deltas.
    #[must_use]
    pub const fn requires_snapshot(self) -> bool {
        self.sequence_tracker.requires_snapshot()
    }
}

/// Request for a bounded deterministic replay snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadEmbeddedReplay {
    pub bar_count: usize,
}

/// Application port implemented by embedded and future transport adapters.
pub trait MarketBarReplayPort {
    type Error;

    /// Loads the initial bounded snapshot.
    ///
    /// # Errors
    ///
    /// Returns adapter or validation failures.
    fn load_snapshot(&self, request: LoadEmbeddedReplay) -> Result<ReplaySnapshot, Self::Error>;

    /// Loads one delta after the accepted predecessor, if data remains.
    ///
    /// # Errors
    ///
    /// Returns adapter, validation, or sequence failures.
    fn load_delta(
        &self,
        previous_sequence: u64,
    ) -> Result<Option<StreamDelta<ProvenancedMarketBar>>, Self::Error>;
}

/// Local Stage 1 source used until the real replay transport is implemented.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EmbeddedReplaySource;

impl MarketBarReplayPort for EmbeddedReplaySource {
    type Error = ReplayValidationError;

    fn load_snapshot(&self, request: LoadEmbeddedReplay) -> Result<ReplaySnapshot, Self::Error> {
        if !(1..=MAX_EMBEDDED_REPLAY_BARS).contains(&request.bar_count) {
            return Err(StreamProtocolError::ItemLimitExceeded {
                requested: request.bar_count,
                maximum: MAX_EMBEDDED_REPLAY_BARS,
            }
            .into());
        }

        ReplaySnapshot::try_new(
            embedded_instrument()?,
            ReplayProvenance::EmbeddedFixture,
            embedded_bar_definition(),
            embedded_bars(request.bar_count),
        )
    }

    fn load_delta(
        &self,
        previous_sequence: u64,
    ) -> Result<Option<StreamDelta<ProvenancedMarketBar>>, Self::Error> {
        let sequence = previous_sequence
            .checked_add(1)
            .ok_or(StreamProtocolError::SequenceOverflow)?;
        let bar_count = usize::try_from(sequence)
            .map_err(|_| StreamProtocolError::ItemCountOverflow(usize::MAX))?;
        if bar_count > MAX_EMBEDDED_REPLAY_BARS {
            return Ok(None);
        }
        let bar = embedded_bars(bar_count)
            .pop()
            .expect("a non-zero sequence creates one embedded bar");
        let item = try_provenanced_market_bar(bar, embedded_event_provenance(&bar))?;
        Ok(Some(StreamDelta::try_new(
            previous_sequence,
            sequence,
            item,
        )?))
    }
}

impl UseCase<LoadEmbeddedReplay> for EmbeddedReplaySource {
    type Output = ReplaySnapshot;
    type Error = ReplayValidationError;

    fn execute(&self, command: LoadEmbeddedReplay) -> Result<Self::Output, Self::Error> {
        self.load_snapshot(command)
    }
}

fn embedded_instrument() -> Result<InstrumentRevision, ReplayValidationError> {
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new("instrument:fixture:axf:xnas")?,
        revision: 1,
        asset_class: AssetClass::Equity,
        symbol: "AXF".to_string(),
        venue_id: "XNAS".to_string(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(2, 0)?,
        lifecycle: InstrumentLifecycle::Active,
    };
    instrument.validate()?;
    Ok(instrument)
}

fn embedded_bar_definition() -> BarDefinition {
    BarDefinition {
        definition_id: "fixture:one_minute:unadjusted:v1".to_string(),
        version: 1,
        interval_seconds: u32::try_from(EMBEDDED_REPLAY_INTERVAL_SECONDS)
            .expect("embedded interval fits u32"),
    }
}

fn embedded_bars(count: usize) -> Vec<MarketBar> {
    let mut bars = Vec::with_capacity(count);
    let mut previous_close = 18_400_i64;
    let mut entropy = 0x6a09_e667_f3bc_c909_u64;

    for index in 0..count {
        entropy = entropy
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let movement = i64::try_from((entropy >> 32) % 121).expect("movement is bounded") - 60;
        let upper_wick = 20 + i64::try_from((entropy >> 16) % 45).expect("upper wick is bounded");
        let lower_wick = 20 + i64::try_from(entropy % 45).expect("lower wick is bounded");
        let close = (previous_close + movement).max(100);
        let index_i64 = i64::try_from(index).expect("replay bound fits i64");
        let source_sequence = u64::try_from(index).expect("replay bound fits u64") + 1;
        let volume = 10_000 + i64::try_from((entropy >> 8) % 90_000).expect("volume is bounded");

        bars.push(MarketBar {
            source_sequence,
            exchange_timestamp_seconds: EMBEDDED_REPLAY_START_SECONDS
                + index_i64 * EMBEDDED_REPLAY_INTERVAL_SECONDS,
            open: previous_close,
            high: previous_close.max(close) + upper_wick,
            low: previous_close.min(close) - lower_wick,
            close,
            volume,
        });
        previous_close = close;
    }

    bars
}

fn embedded_event_provenance(bar: &MarketBar) -> MarketEventProvenance {
    let exchange_timestamp_unix_nanos =
        bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
    MarketEventProvenance {
        event_id: format!("embedded_market_bar_{}", bar.source_sequence),
        event_time_unix_nanos: exchange_timestamp_unix_nanos,
        publication_time_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(4_000),
        producer: "embedded_replay_source".to_string(),
        schema_version: 1,
        correlation_id: "embedded_replay".to_string(),
        causation_id: String::new(),
        entitlement_revision: "embedded_fixture_entitlement_v1".to_string(),
        partition_id: 0,
        ownership_epoch: 1,
        source_id: "embedded_fixture".to_string(),
        source_sequence: bar.source_sequence,
        exchange_timestamp_unix_nanos,
        provider_receive_timestamp_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(1_000),
        nic_receive_timestamp_unix_nanos: None,
        axiusflow_receive_timestamp_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(2_000),
        normalized_timestamp_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(3_000),
        fanout_enqueue_timestamp_unix_nanos: None,
        correction_flags: 0,
        quality_flags: 0,
        nic_timestamp_source: 0,
        semantic_class: 2,
    }
}

/// Immutable application generation published atomically by one model writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketGeneration<T> {
    partition_id: u32,
    ownership_epoch: NonZeroU64,
    generation: NonZeroU64,
    first_sequence: NonZeroU64,
    last_sequence: NonZeroU64,
    items: Arc<[T]>,
}

impl<T> MarketGeneration<T> {
    /// Creates a non-empty, contiguous immutable generation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero values, an empty generation, or a sequence/count mismatch.
    pub fn try_new(
        partition_id: u32,
        ownership_epoch: u64,
        generation: u64,
        first_sequence: u64,
        last_sequence: u64,
        items: Vec<T>,
    ) -> Result<Self, StreamProtocolError> {
        let ownership_epoch =
            NonZeroU64::new(ownership_epoch).ok_or(StreamProtocolError::ZeroSequence)?;
        let generation = NonZeroU64::new(generation).ok_or(StreamProtocolError::ZeroSequence)?;
        let first_sequence =
            NonZeroU64::new(first_sequence).ok_or(StreamProtocolError::ZeroSequence)?;
        let last_sequence =
            NonZeroU64::new(last_sequence).ok_or(StreamProtocolError::ZeroSequence)?;
        if items.is_empty() {
            return Err(StreamProtocolError::EmptySnapshot);
        }
        let expected = last_sequence
            .get()
            .checked_sub(first_sequence.get())
            .and_then(|difference| difference.checked_add(1))
            .ok_or(StreamProtocolError::InvalidSequenceRange {
                first: first_sequence.get(),
                last: last_sequence.get(),
            })?;
        if expected != u64::try_from(items.len()).unwrap_or(u64::MAX) {
            return Err(StreamProtocolError::ItemCountMismatch {
                expected,
                actual: items.len(),
            });
        }
        Ok(Self {
            partition_id,
            ownership_epoch,
            generation,
            first_sequence,
            last_sequence,
            items: items.into(),
        })
    }

    #[must_use]
    pub const fn partition_id(&self) -> u32 {
        self.partition_id
    }

    #[must_use]
    pub const fn ownership_epoch(&self) -> u64 {
        self.ownership_epoch.get()
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation.get()
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    #[must_use]
    pub const fn sequence_range(&self) -> (u64, u64) {
        (self.first_sequence.get(), self.last_sequence.get())
    }
}

/// Result of applying one validated market-bar update to the client model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketBarModelOutcome {
    /// A new immutable generation is ready for atomic publication to readers.
    Published(MarketGeneration<ProvenancedMarketBar>),
    /// The update was already represented by the current generation.
    Duplicate,
    /// Ordered mutation is blocked until a replacement snapshot is installed.
    ResnapshotRequired(ResnapshotReason),
}

/// Bounded single-writer client projection for one market-bar partition.
///
/// The owner feeds transport-neutral snapshots and deltas into this model from one
/// background worker. Readers receive cloned immutable generations and never observe
/// partially applied snapshots, deltas, ownership handoffs, or history eviction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketBarClientModel {
    maximum_items: std::num::NonZeroUsize,
    session: Option<ReplaySession>,
    current: Option<MarketGeneration<ProvenancedMarketBar>>,
    current_instrument: Option<InstrumentRevision>,
    current_bar_definition: Option<BarDefinition>,
    pending_resnapshot: Option<ResnapshotReason>,
}

impl MarketBarClientModel {
    /// Creates an empty model with an explicit retained-history bound.
    #[must_use]
    pub const fn new(maximum_items: std::num::NonZeroUsize) -> Self {
        Self {
            maximum_items,
            session: None,
            current: None,
            current_instrument: None,
            current_bar_definition: None,
            pending_resnapshot: Some(ResnapshotReason::InitialSubscription),
        }
    }

    /// Applies one validated update atomically and publishes at most one generation.
    ///
    /// Accepted deltas are validated in order before bounded oldest-item eviction.
    /// Gaps and ownership changes preserve the last immutable generation for readers
    /// but block all subsequent deltas until a replacement snapshot is installed.
    ///
    /// # Errors
    ///
    /// Returns an error when replay validation fails or when the immutable
    /// generation counter overflows.
    pub fn apply_update(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => self.install_snapshot(&snapshot),
            ReplayStreamUpdate::Delta(delta) => self.apply_delta(&delta),
        }
    }

    fn validate_snapshot_transition(
        &self,
        snapshot: &ReplaySnapshot,
    ) -> Result<(), ReplayValidationError> {
        let Some(current) = self.current.as_ref() else {
            return Ok(());
        };
        let evidence = snapshot.evidence();
        if evidence.partition_id != current.partition_id() {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "partition_id",
            ));
        }
        if self.current_instrument.as_ref() != Some(snapshot.instrument())
            || self.current_bar_definition.as_ref() != Some(snapshot.bar_definition())
        {
            return Err(ReplayValidationError::SnapshotEvidenceMismatch(
                "series_identity",
            ));
        }
        if evidence.ownership_epoch < current.ownership_epoch() {
            return Err(ReplayValidationError::SnapshotOwnershipRegression {
                current_epoch: current.ownership_epoch(),
                actual_epoch: evidence.ownership_epoch,
            });
        }
        if evidence.ownership_epoch == current.ownership_epoch()
            && (evidence.generation <= current.generation()
                || evidence.last_sequence < current.sequence_range().1)
        {
            return Err(ReplayValidationError::StaleSnapshot {
                current_generation: current.generation(),
                current_last_sequence: current.sequence_range().1,
                actual_generation: evidence.generation,
                actual_last_sequence: evidence.last_sequence,
            });
        }
        Ok(())
    }

    fn install_snapshot(
        &mut self,
        snapshot: &ReplaySnapshot,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        self.validate_snapshot_transition(snapshot)?;
        let session = ReplaySession::try_new(snapshot)?;
        let evidence = snapshot.evidence();
        let retained_start = snapshot
            .bars()
            .len()
            .saturating_sub(self.maximum_items.get());
        let retained_items = snapshot.bars()[retained_start..].to_vec();
        let first_sequence = retained_items
            .first()
            .ok_or(StreamProtocolError::EmptySnapshot)?
            .value()
            .source_sequence;
        let generation = MarketGeneration::try_new(
            evidence.partition_id,
            evidence.ownership_epoch,
            evidence.generation,
            first_sequence,
            evidence.last_sequence,
            retained_items,
        )?;
        self.session = Some(session);
        self.current = Some(generation.clone());
        self.current_instrument = Some(snapshot.instrument().clone());
        self.current_bar_definition = Some(snapshot.bar_definition().clone());
        self.pending_resnapshot = None;
        Ok(MarketBarModelOutcome::Published(generation))
    }

    fn apply_delta(
        &mut self,
        delta: &StreamDelta<ProvenancedMarketBar>,
    ) -> Result<MarketBarModelOutcome, ReplayValidationError> {
        let Some(current) = self.current.as_ref() else {
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                self.pending_resnapshot
                    .unwrap_or(ResnapshotReason::InitialSubscription),
            ));
        };
        let provenance = delta.item().provenance();
        if provenance.partition_id != current.partition_id()
            || provenance.ownership_epoch != current.ownership_epoch()
        {
            self.session = None;
            self.pending_resnapshot = Some(ResnapshotReason::OwnershipHandoff);
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                ResnapshotReason::OwnershipHandoff,
            ));
        }
        let current_schema_version = current
            .items()
            .first()
            .ok_or(StreamProtocolError::EmptySnapshot)?
            .provenance()
            .schema_version;
        if provenance.schema_version != current_schema_version {
            self.session = None;
            self.pending_resnapshot = Some(ResnapshotReason::SchemaChanged);
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                ResnapshotReason::SchemaChanged,
            ));
        }
        let Some(mut candidate_session) = self.session else {
            return Ok(MarketBarModelOutcome::ResnapshotRequired(
                self.pending_resnapshot
                    .unwrap_or(ResnapshotReason::SequenceGap),
            ));
        };
        match candidate_session.accept_delta(delta)? {
            SequenceDecision::Duplicate => Ok(MarketBarModelOutcome::Duplicate),
            SequenceDecision::Gap { .. } | SequenceDecision::SnapshotRequired => {
                self.session = Some(candidate_session);
                let reason = self
                    .pending_resnapshot
                    .unwrap_or(ResnapshotReason::SequenceGap);
                self.pending_resnapshot = Some(reason);
                Ok(MarketBarModelOutcome::ResnapshotRequired(reason))
            }
            SequenceDecision::Accepted => {
                let next_generation = current
                    .generation()
                    .checked_add(1)
                    .ok_or(StreamProtocolError::SequenceOverflow)?;
                let mut items = current.items().to_vec();
                items.push(delta.item().clone());
                let evicted = items.len().saturating_sub(self.maximum_items.get());
                items.drain(..evicted);
                let first_sequence = items
                    .first()
                    .ok_or(StreamProtocolError::EmptySnapshot)?
                    .value()
                    .source_sequence;
                let last_sequence = items
                    .last()
                    .ok_or(StreamProtocolError::EmptySnapshot)?
                    .value()
                    .source_sequence;
                let generation = MarketGeneration::try_new(
                    current.partition_id(),
                    current.ownership_epoch(),
                    next_generation,
                    first_sequence,
                    last_sequence,
                    items,
                )?;
                self.session = Some(candidate_session);
                self.current = Some(generation.clone());
                self.pending_resnapshot = None;
                Ok(MarketBarModelOutcome::Published(generation))
            }
        }
    }

    /// Explicitly blocks ordered deltas while preserving the last immutable generation.
    ///
    /// Connected adapters call this after transport loss, protocol failure, or bounded
    /// queue overflow. A subsequent validated snapshot atomically clears the latch.
    pub fn require_resnapshot(&mut self, reason: ResnapshotReason) {
        self.session = None;
        self.pending_resnapshot = Some(reason);
    }

    /// Returns the latest immutable generation, if an initial snapshot was installed.
    #[must_use]
    pub const fn current_generation(&self) -> Option<&MarketGeneration<ProvenancedMarketBar>> {
        self.current.as_ref()
    }

    /// Returns whether ordered updates are blocked pending a replacement snapshot.
    #[must_use]
    pub const fn requires_snapshot(&self) -> bool {
        self.pending_resnapshot.is_some()
    }

    /// Returns the active recovery reason, if mutation is currently blocked.
    #[must_use]
    pub const fn pending_resnapshot_reason(&self) -> Option<ResnapshotReason> {
        self.pending_resnapshot
    }
}

/// Reason a consumer requests a bounded atomic replacement snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResnapshotReason {
    InitialSubscription,
    SequenceGap,
    SchemaChanged,
    QueueOverflow,
    OwnershipHandoff,
    TransportReset,
}

/// One transport-neutral publication with its validated immutable model generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketStreamPublication {
    subscription_id: String,
    update: ReplayStreamUpdate,
    generation: MarketGeneration<ProvenancedMarketBar>,
    predecessor_generation: Option<u64>,
}

impl MarketStreamPublication {
    /// Creates a publication only when the update and generation describe the same state.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty subscription or contradictory update/generation evidence.
    pub fn try_new(
        subscription_id: String,
        update: ReplayStreamUpdate,
        generation: MarketGeneration<ProvenancedMarketBar>,
    ) -> Result<Self, MarketStreamPublicationError> {
        if subscription_id.is_empty() {
            return Err(MarketStreamPublicationError::EmptySubscriptionId);
        }
        let predecessor_generation = validate_stream_publication(&update, &generation)?;
        Ok(Self {
            subscription_id,
            update,
            generation,
            predecessor_generation,
        })
    }

    #[must_use]
    pub fn subscription_id(&self) -> &str {
        &self.subscription_id
    }

    #[must_use]
    pub const fn update(&self) -> &ReplayStreamUpdate {
        &self.update
    }

    #[must_use]
    pub const fn generation(&self) -> &MarketGeneration<ProvenancedMarketBar> {
        &self.generation
    }

    #[must_use]
    pub const fn predecessor_generation(&self) -> Option<u64> {
        self.predecessor_generation
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        String,
        ReplayStreamUpdate,
        MarketGeneration<ProvenancedMarketBar>,
        Option<u64>,
    ) {
        (
            self.subscription_id,
            self.update,
            self.generation,
            self.predecessor_generation,
        )
    }
}

/// Contradictory evidence at the neutral stream publication boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketStreamPublicationError {
    EmptySubscriptionId,
    UpdateGenerationMismatch(&'static str),
}

impl fmt::Display for MarketStreamPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid market stream publication: {self:?}")
    }
}

impl Error for MarketStreamPublicationError {}

fn validate_stream_publication(
    update: &ReplayStreamUpdate,
    generation: &MarketGeneration<ProvenancedMarketBar>,
) -> Result<Option<u64>, MarketStreamPublicationError> {
    let mismatch = |field| MarketStreamPublicationError::UpdateGenerationMismatch(field);
    let (generation_first, generation_last) = generation.sequence_range();
    let predecessor_generation = match update {
        ReplayStreamUpdate::Snapshot(snapshot) => {
            let evidence = snapshot.evidence();
            if generation.partition_id() != evidence.partition_id {
                return Err(mismatch("snapshot partition"));
            }
            if generation.ownership_epoch() != evidence.ownership_epoch {
                return Err(mismatch("snapshot ownership epoch"));
            }
            if generation.generation() != evidence.generation {
                return Err(mismatch("snapshot generation"));
            }
            if generation_last != evidence.last_sequence {
                return Err(mismatch("snapshot last sequence"));
            }
            let retained_start = snapshot
                .bars()
                .len()
                .checked_sub(generation.items().len())
                .ok_or_else(|| mismatch("snapshot retained item count"))?;
            if generation.items() != &snapshot.bars()[retained_start..] {
                return Err(mismatch("snapshot retained items"));
            }
            None
        }
        ReplayStreamUpdate::Delta(delta) => {
            let provenance = delta.item().provenance();
            if generation.partition_id() != provenance.partition_id {
                return Err(mismatch("delta partition"));
            }
            if generation.ownership_epoch() != provenance.ownership_epoch {
                return Err(mismatch("delta ownership epoch"));
            }
            if generation_last != delta.sequence() {
                return Err(mismatch("delta last sequence"));
            }
            if generation.items().last() != Some(delta.item()) {
                return Err(mismatch("delta latest item"));
            }
            Some(
                generation
                    .generation()
                    .checked_sub(1)
                    .filter(|predecessor| *predecessor != 0)
                    .ok_or_else(|| mismatch("delta predecessor generation"))?,
            )
        }
    };
    let first_item_sequence = generation
        .items()
        .first()
        .map(Provenanced::provenance)
        .map(|provenance| provenance.source_sequence);
    let last_item_sequence = generation
        .items()
        .last()
        .map(Provenanced::provenance)
        .map(|provenance| provenance.source_sequence);
    if first_item_sequence != Some(generation_first) || last_item_sequence != Some(generation_last)
    {
        return Err(mismatch("generation item sequence range"));
    }
    Ok(predecessor_generation)
}

/// Commands accepted by a bounded market-stream runtime port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketStreamCommand {
    Connect,
    Recover {
        request_id: u64,
        reason: ResnapshotReason,
    },
    Shutdown,
}

/// Result of offering one command without blocking or increasing capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketStreamCommandOffer {
    Accepted,
    Full,
}

/// Transport keepalive activity exposed without binding consumers to one wire protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketStreamControlSignal {
    KeepaliveRequest,
    KeepaliveResponse,
}

/// Provider-neutral lifecycle output from one bounded market-stream runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketStreamEvent {
    Connected {
        connection_epoch: u64,
        attempt: usize,
    },
    Publication(Box<MarketStreamPublication>),
    RecoverySnapshot {
        request_id: u64,
        publication: Box<MarketStreamPublication>,
    },
    Control(MarketStreamControlSignal),
    StreamInvalid {
        connection_epoch: Option<u64>,
        reason: ResnapshotReason,
        dropped_events: usize,
    },
    RecoveryAttemptFailed {
        request_id: Option<u64>,
        attempt: usize,
        remaining: usize,
    },
    RecoveryExhausted {
        request_id: Option<u64>,
        attempts: usize,
        reason: ResnapshotReason,
        dropped_events: usize,
    },
    RecoveryRejected {
        request_id: u64,
        active_request_id: u64,
    },
    Stopped {
        graceful: bool,
        dropped_events: usize,
    },
}

/// Bounded provider-neutral command/event boundary implemented by stream adapters.
pub trait MarketStreamRuntimePort {
    type Error;

    /// Offers one command without blocking or hidden buffering.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when the runtime command boundary is disconnected.
    fn try_send_stream_command(
        &self,
        command: MarketStreamCommand,
    ) -> Result<MarketStreamCommandOffer, Self::Error>;

    /// Receives at most one ready event without blocking.
    ///
    /// `Ok(None)` means no event is currently ready and does not imply disconnection.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when the runtime failed or disconnected.
    fn try_recv_stream_event(&self) -> Result<Option<MarketStreamEvent>, Self::Error>;
}

/// Transport-neutral live/replay model port consumed by background application workers.
pub trait MarketGenerationPort<T> {
    type Error;

    /// Loads a bounded immutable generation suitable for atomic publication.
    ///
    /// # Errors
    ///
    /// Returns adapter, entitlement, validation, or availability failures.
    fn load_generation(&self, reason: ResnapshotReason)
    -> Result<MarketGeneration<T>, Self::Error>;
}

/// Validation failures at the replay application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayValidationError {
    Instrument(InstrumentValidationError),
    MarketData(MarketDataValidationError),
    Stream(StreamProtocolError),
    NonIncreasingTimestamp {
        source_sequence: u64,
    },
    ProvenanceSequenceMismatch {
        bar: u64,
        provenance: u64,
    },
    ExchangeTimestampOverflow {
        source_sequence: u64,
    },
    ProvenanceExchangeTimestampMismatch {
        source_sequence: u64,
        bar_seconds: i64,
        provenance_nanos: i64,
    },
    MissingProvenance(&'static str),
    InvalidProvenanceRevision,
    InvalidSnapshotEvidence(&'static str),
    SnapshotEvidenceMismatch(&'static str),
    SnapshotChecksumMismatch,
    SnapshotOwnershipRegression {
        current_epoch: u64,
        actual_epoch: u64,
    },
    StaleSnapshot {
        current_generation: u64,
        current_last_sequence: u64,
        actual_generation: u64,
        actual_last_sequence: u64,
    },
    UncorrelatedRecoverySnapshot {
        request_id: u64,
    },
}

impl From<InstrumentValidationError> for ReplayValidationError {
    fn from(error: InstrumentValidationError) -> Self {
        Self::Instrument(error)
    }
}

impl From<MarketDataValidationError> for ReplayValidationError {
    fn from(error: MarketDataValidationError) -> Self {
        Self::MarketData(error)
    }
}

impl From<StreamProtocolError> for ReplayValidationError {
    fn from(error: StreamProtocolError) -> Self {
        Self::Stream(error)
    }
}

impl fmt::Display for ReplayValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instrument(error) => write!(formatter, "invalid replay instrument: {error}"),
            Self::MarketData(error) => write!(formatter, "invalid replay market data: {error}"),
            Self::Stream(error) => write!(formatter, "invalid replay stream: {error}"),
            Self::NonIncreasingTimestamp { source_sequence } => write!(
                formatter,
                "replay timestamp did not increase at source sequence {source_sequence}"
            ),
            Self::ProvenanceSequenceMismatch { bar, provenance } => write!(
                formatter,
                "bar sequence {bar} does not match provenance sequence {provenance}"
            ),
            Self::ExchangeTimestampOverflow { source_sequence } => write!(
                formatter,
                "bar exchange timestamp overflows nanoseconds at source sequence {source_sequence}"
            ),
            Self::ProvenanceExchangeTimestampMismatch {
                source_sequence,
                bar_seconds,
                provenance_nanos,
            } => write!(
                formatter,
                "bar exchange timestamp {bar_seconds}s does not match provenance timestamp {provenance_nanos}ns at source sequence {source_sequence}"
            ),
            Self::MissingProvenance(field) => {
                write!(
                    formatter,
                    "market provenance field {field} must not be empty"
                )
            }
            Self::InvalidProvenanceRevision => formatter
                .write_str("market provenance ownership epoch and schema version must be non-zero"),
            Self::InvalidSnapshotEvidence(field) => {
                write!(
                    formatter,
                    "snapshot evidence field {field} must be non-zero"
                )
            }
            Self::SnapshotEvidenceMismatch(field) => write!(
                formatter,
                "snapshot evidence field {field} does not match its market values"
            ),
            Self::SnapshotChecksumMismatch => {
                formatter.write_str("snapshot checksum does not match its canonical market values")
            }
            Self::SnapshotOwnershipRegression {
                current_epoch,
                actual_epoch,
            } => write!(
                formatter,
                "snapshot ownership epoch {actual_epoch} regresses current epoch {current_epoch}"
            ),
            Self::StaleSnapshot {
                current_generation,
                current_last_sequence,
                actual_generation,
                actual_last_sequence,
            } => write!(
                formatter,
                "snapshot generation {actual_generation} sequence {actual_last_sequence} does not advance current generation {current_generation} sequence {current_last_sequence}"
            ),
            Self::UncorrelatedRecoverySnapshot { request_id } => write!(
                formatter,
                "snapshot cannot bypass active recovery request {request_id}"
            ),
        }
    }
}

impl Error for ReplayValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Instrument(error) => Some(error),
            Self::MarketData(error) => Some(error),
            Self::Stream(error) => Some(error),
            Self::NonIncreasingTimestamp { .. }
            | Self::ProvenanceSequenceMismatch { .. }
            | Self::ExchangeTimestampOverflow { .. }
            | Self::ProvenanceExchangeTimestampMismatch { .. }
            | Self::MissingProvenance(_)
            | Self::InvalidProvenanceRevision
            | Self::InvalidSnapshotEvidence(_)
            | Self::SnapshotEvidenceMismatch(_)
            | Self::SnapshotChecksumMismatch
            | Self::SnapshotOwnershipRegression { .. }
            | Self::StaleSnapshot { .. }
            | Self::UncorrelatedRecoverySnapshot { .. } => None,
        }
    }
}
