//! Bounded market-bar stream decoding with atomic snapshot assembly.
//!
//! This module owns the stream decoder state machine: subscription and series context
//! validation, chunked snapshot accumulation, sequence and gap policy, and the bounded
//! binary decoder that projects frames into validated replay updates.

use crate::errors::{BinaryMarketStreamError, ProtobufAdapterError};
use crate::wire_codec::{try_decode_market_bar_delta, try_decode_market_bar_snapshot};
use crate::{DecimalConvention, MAX_MARKET_BAR_SNAPSHOT_CHUNKS};
use axiusflow_application::{
    ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate, validate_provenanced_market_bar,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_protocols::{
    MAX_STREAM_SNAPSHOT_ITEMS, Provenanced, SnapshotEvidence, StreamDelta, StreamProtocolError,
    StreamSnapshot,
    protobuf::axiusflow::{
        instrument::v1 as instrument_wire,
        market::v1::{self as market_wire, market_bar_stream_envelope},
    },
};
use axiusflow_transport::BoundedBinaryFrameDecoder;
use prost::Message;
use std::num::NonZeroUsize;

/// A decoded snapshot with validated subscription and series context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedMarketBarSnapshot {
    pub subscription_id: String,
    pub instrument: InstrumentRevision,
    pub bar_definition: BarDefinition,
    pub stream: StreamSnapshot<Provenanced<MarketBar>>,
    pub evidence: SnapshotEvidence,
}

/// A decoded delta with its validated series definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedMarketBarDelta {
    pub subscription_id: String,
    pub bar_definition: BarDefinition,
    pub delta: StreamDelta<Provenanced<MarketBar>>,
}

/// A validated market-bar stream update projected into domain values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedMarketBarStreamUpdate {
    Snapshot(DecodedMarketBarSnapshot),
    Delta(DecodedMarketBarDelta),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MarketBarStreamContext {
    subscription_id: String,
    instrument: InstrumentRevision,
    bar_definition: BarDefinition,
    session_generation: u64,
    publication_generation: u64,
    schema_version: u32,
    last_sequence: u64,
    last_exchange_timestamp_seconds: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingMarketBarSnapshot {
    subscription_id: String,
    snapshot_id: String,
    instrument: Option<instrument_wire::InstrumentRevision>,
    first_sequence: u64,
    last_sequence: u64,
    session_generation: u64,
    publication_generation: u64,
    checksum: Vec<u8>,
    schema_version: u32,
    chunk_count: u32,
    total_item_count: usize,
    next_chunk_index: u32,
    bars: Vec<market_wire::MarketBar>,
}

impl PendingMarketBarSnapshot {
    fn starts_with(wire: market_wire::MarketBarSnapshot) -> Self {
        let bar_capacity = usize::try_from(wire.total_item_count).unwrap_or(usize::MAX);
        let mut bars = Vec::with_capacity(bar_capacity);
        bars.extend(wire.bars);
        Self {
            subscription_id: wire.subscription_id,
            snapshot_id: wire.snapshot_id,
            instrument: wire.instrument,
            first_sequence: wire.first_sequence,
            last_sequence: wire.last_sequence,
            session_generation: wire.session_generation,
            publication_generation: wire.publication_generation,
            checksum: wire.checksum,
            schema_version: wire.schema_version,
            chunk_count: wire.chunk_count,
            total_item_count: bar_capacity,
            next_chunk_index: 1,
            bars,
        }
    }

    fn matches(&self, wire: &market_wire::MarketBarSnapshot) -> bool {
        self.subscription_id == wire.subscription_id
            && self.snapshot_id == wire.snapshot_id
            && self.instrument == wire.instrument
            && self.first_sequence == wire.first_sequence
            && self.last_sequence == wire.last_sequence
            && self.session_generation == wire.session_generation
            && self.publication_generation == wire.publication_generation
            && self.checksum == wire.checksum
            && self.schema_version == wire.schema_version
            && self.chunk_count == wire.chunk_count
            && u32::try_from(self.total_item_count).ok() == Some(wire.total_item_count)
    }

    fn into_complete_wire(self) -> market_wire::MarketBarSnapshot {
        market_wire::MarketBarSnapshot {
            subscription_id: self.subscription_id,
            instrument: self.instrument,
            bars: self.bars,
            first_sequence: self.first_sequence,
            last_sequence: self.last_sequence,
            session_generation: self.session_generation,
            publication_generation: self.publication_generation,
            checksum: self.checksum,
            schema_version: self.schema_version,
            snapshot_id: String::new(),
            chunk_index: 0,
            chunk_count: 0,
            total_item_count: 0,
        }
    }
}

fn validate_snapshot_chunk(
    wire: &market_wire::MarketBarSnapshot,
) -> Result<(usize, usize), ProtobufAdapterError> {
    if wire.snapshot_id.trim().is_empty() {
        return Err(ProtobufAdapterError::EmptySnapshotId);
    }
    let chunk_count = usize::try_from(wire.chunk_count).map_err(|_| {
        ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
            actual: usize::MAX,
            maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
        }
    })?;
    if chunk_count == 0 || chunk_count > MAX_MARKET_BAR_SNAPSHOT_CHUNKS {
        return Err(ProtobufAdapterError::SnapshotChunkCountLimitExceeded {
            actual: chunk_count,
            maximum: MAX_MARKET_BAR_SNAPSHOT_CHUNKS,
        });
    }
    if wire.chunk_index >= wire.chunk_count {
        return Err(ProtobufAdapterError::SnapshotChunkIndexOutOfRange {
            index: wire.chunk_index,
            count: wire.chunk_count,
        });
    }
    let total_item_count = usize::try_from(wire.total_item_count).map_err(|_| {
        ProtobufAdapterError::SnapshotTotalItemCountInvalid {
            actual: usize::MAX,
            maximum: MAX_STREAM_SNAPSHOT_ITEMS,
        }
    })?;
    if total_item_count == 0 || total_item_count > MAX_STREAM_SNAPSHOT_ITEMS {
        return Err(ProtobufAdapterError::SnapshotTotalItemCountInvalid {
            actual: total_item_count,
            maximum: MAX_STREAM_SNAPSHOT_ITEMS,
        });
    }
    if chunk_count > total_item_count {
        return Err(ProtobufAdapterError::SnapshotChunkCountExceedsItems {
            chunks: chunk_count,
            items: total_item_count,
        });
    }
    if wire.bars.is_empty() {
        return Err(ProtobufAdapterError::EmptySnapshotChunk);
    }
    let range_count = wire
        .last_sequence
        .checked_sub(wire.first_sequence)
        .and_then(|difference| difference.checked_add(1))
        .ok_or(StreamProtocolError::InvalidSequenceRange {
            first: wire.first_sequence,
            last: wire.last_sequence,
        })?;
    if range_count != u64::from(wire.total_item_count) {
        return Err(StreamProtocolError::ItemCountMismatch {
            expected: range_count,
            actual: total_item_count,
        }
        .into());
    }
    Ok((chunk_count, total_item_count))
}

fn accept_snapshot_chunk(
    pending: &mut Option<PendingMarketBarSnapshot>,
    wire: market_wire::MarketBarSnapshot,
) -> Result<Option<market_wire::MarketBarSnapshot>, ProtobufAdapterError> {
    let (chunk_count, total_item_count) = validate_snapshot_chunk(&wire)?;
    if let Some(assembly) = pending.as_mut() {
        if !assembly.matches(&wire) {
            return Err(ProtobufAdapterError::SnapshotChunkIdentityMismatch);
        }
        if wire.chunk_index != assembly.next_chunk_index {
            return Err(ProtobufAdapterError::UnexpectedSnapshotChunkIndex {
                expected: assembly.next_chunk_index,
                actual: wire.chunk_index,
            });
        }
        let resulting_items = assembly
            .bars
            .len()
            .checked_add(wire.bars.len())
            .ok_or(ProtobufAdapterError::SnapshotAssemblyItemLimitExceeded)?;
        let is_final = wire.chunk_index.checked_add(1) == Some(wire.chunk_count);
        if resulting_items > total_item_count
            || (is_final && resulting_items != total_item_count)
            || (!is_final && resulting_items >= total_item_count)
        {
            return Err(ProtobufAdapterError::SnapshotAssemblyItemCountMismatch {
                expected: total_item_count,
                actual: resulting_items,
            });
        }
        assembly.bars.extend(wire.bars);
        assembly.next_chunk_index = assembly
            .next_chunk_index
            .checked_add(1)
            .ok_or(ProtobufAdapterError::SnapshotAssemblyItemLimitExceeded)?;
    } else {
        if wire.chunk_index != 0 {
            return Err(ProtobufAdapterError::UnexpectedSnapshotChunkIndex {
                expected: 0,
                actual: wire.chunk_index,
            });
        }
        let is_final = chunk_count == 1;
        if wire.bars.len() > total_item_count
            || (is_final && wire.bars.len() != total_item_count)
            || (!is_final && wire.bars.len() >= total_item_count)
        {
            return Err(ProtobufAdapterError::SnapshotAssemblyItemCountMismatch {
                expected: total_item_count,
                actual: wire.bars.len(),
            });
        }
        *pending = Some(PendingMarketBarSnapshot::starts_with(wire));
    }

    let complete = pending
        .as_ref()
        .is_some_and(|assembly| assembly.next_chunk_index == assembly.chunk_count);
    if !complete {
        return Ok(None);
    }
    let assembly = pending
        .take()
        .ok_or(ProtobufAdapterError::EmptySnapshotChunk)?;
    if assembly.bars.len() != assembly.total_item_count {
        return Err(ProtobufAdapterError::SnapshotAssemblyItemCountMismatch {
            expected: assembly.total_item_count,
            actual: assembly.bars.len(),
        });
    }
    Ok(Some(assembly.into_complete_wire()))
}

/// Stateful decoder that enforces snapshot-before-delta stream semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct MarketBarStreamDecoder {
    decimal_convention: DecimalConvention,
    context: Option<MarketBarStreamContext>,
    pending_snapshot: Option<PendingMarketBarSnapshot>,
}

impl MarketBarStreamDecoder {
    /// Creates a decoder with explicit decimal-unit expectations.
    #[must_use]
    pub const fn new(decimal_convention: DecimalConvention) -> Self {
        Self {
            decimal_convention,
            context: None,
            pending_snapshot: None,
        }
    }

    /// Decodes one generated stream envelope and advances accepted stream state.
    ///
    /// Chunked snapshots return `Ok(None)` until the final chunk validates and
    /// installs one complete snapshot atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed snapshot assembly, a missing update, invalid
    /// snapshot or delta, a delta before a snapshot, or inconsistent stream state.
    pub fn try_decode(
        &mut self,
        envelope: market_wire::MarketBarStreamEnvelope,
    ) -> Result<Option<DecodedMarketBarStreamUpdate>, ProtobufAdapterError> {
        match envelope.update.ok_or(ProtobufAdapterError::MissingUpdate)? {
            market_bar_stream_envelope::Update::Snapshot(snapshot) => {
                if snapshot.chunk_count == 0 {
                    if self.pending_snapshot.is_some() {
                        return Err(ProtobufAdapterError::SnapshotDuringSnapshotAssembly);
                    }
                    let decoded =
                        try_decode_market_bar_snapshot(snapshot, &self.decimal_convention)?;
                    self.install_snapshot_context(&decoded)?;
                    return Ok(Some(DecodedMarketBarStreamUpdate::Snapshot(decoded)));
                }
                let mut candidate = self.pending_snapshot.clone();
                let complete = accept_snapshot_chunk(&mut candidate, snapshot)?;
                let Some(complete) = complete else {
                    self.pending_snapshot = candidate;
                    return Ok(None);
                };
                let decoded = try_decode_market_bar_snapshot(complete, &self.decimal_convention)?;
                self.install_snapshot_context(&decoded)?;
                self.pending_snapshot = None;
                Ok(Some(DecodedMarketBarStreamUpdate::Snapshot(decoded)))
            }
            market_bar_stream_envelope::Update::Delta(delta) => {
                if self.pending_snapshot.is_some() {
                    return Err(ProtobufAdapterError::DeltaDuringSnapshotAssembly);
                }
                let context = self
                    .context
                    .as_ref()
                    .ok_or(ProtobufAdapterError::DeltaBeforeSnapshot)?;
                let decoded = try_decode_market_bar_delta(
                    delta,
                    &context.instrument,
                    &context.bar_definition,
                    &self.decimal_convention,
                )?;
                if decoded.subscription_id != context.subscription_id {
                    return Err(ProtobufAdapterError::SubscriptionMismatch {
                        expected: context.subscription_id.clone(),
                        actual: decoded.subscription_id,
                    });
                }
                if decoded.delta.item().provenance().session_generation
                    != context.session_generation
                {
                    return Err(ProtobufAdapterError::StreamSessionChanged);
                }
                let actual_schema = decoded.delta.item().provenance().schema_version;
                if actual_schema != context.schema_version {
                    return Err(ProtobufAdapterError::StreamSchemaChanged {
                        expected: context.schema_version,
                        actual: actual_schema,
                    });
                }
                if decoded.delta.previous_sequence() != context.last_sequence {
                    return Err(ProtobufAdapterError::UnexpectedPreviousSequence {
                        expected: context.last_sequence,
                        actual: decoded.delta.previous_sequence(),
                    });
                }
                if decoded.delta.item().value().exchange_timestamp_seconds
                    <= context.last_exchange_timestamp_seconds
                {
                    return Err(ProtobufAdapterError::NonIncreasingTimestamp {
                        source_sequence: decoded.delta.sequence(),
                    });
                }

                let Some(next_context) = self.context.as_mut() else {
                    return Err(ProtobufAdapterError::DeltaBeforeSnapshot);
                };
                next_context.last_sequence = decoded.delta.sequence();
                next_context.last_exchange_timestamp_seconds =
                    decoded.delta.item().value().exchange_timestamp_seconds;
                Ok(Some(DecodedMarketBarStreamUpdate::Delta(decoded)))
            }
        }
    }

    fn install_snapshot_context(
        &mut self,
        decoded: &DecodedMarketBarSnapshot,
    ) -> Result<(), ProtobufAdapterError> {
        if let Some(context) = &self.context {
            if decoded.subscription_id != context.subscription_id {
                return Err(ProtobufAdapterError::SubscriptionMismatch {
                    expected: context.subscription_id.clone(),
                    actual: decoded.subscription_id.clone(),
                });
            }
            if decoded.instrument != context.instrument
                || decoded.bar_definition != context.bar_definition
            {
                return Err(ProtobufAdapterError::SnapshotSeriesChanged);
            }
            if decoded.evidence.session_generation < context.session_generation {
                return Err(ProtobufAdapterError::SnapshotSessionGenerationRegression {
                    current: context.session_generation,
                    actual: decoded.evidence.session_generation,
                });
            }
            if decoded.evidence.session_generation == context.session_generation
                && (decoded.evidence.publication_generation <= context.publication_generation
                    || decoded.evidence.last_sequence < context.last_sequence)
            {
                return Err(ProtobufAdapterError::StaleSnapshotTransition {
                    current_generation: context.publication_generation,
                    current_last_sequence: context.last_sequence,
                    actual_generation: decoded.evidence.publication_generation,
                    actual_last_sequence: decoded.evidence.last_sequence,
                });
            }
        }
        let last_item = decoded
            .stream
            .items()
            .last()
            .ok_or(StreamProtocolError::EmptySnapshot)?;
        self.context = Some(MarketBarStreamContext {
            subscription_id: decoded.subscription_id.clone(),
            instrument: decoded.instrument.clone(),
            bar_definition: decoded.bar_definition.clone(),
            session_generation: last_item.provenance().session_generation,
            publication_generation: decoded.evidence.publication_generation,
            schema_version: decoded.evidence.schema_version,
            last_sequence: decoded.stream.last_sequence(),
            last_exchange_timestamp_seconds: last_item.value().exchange_timestamp_seconds,
        });
        Ok(())
    }

    /// Clears installed and partially assembled snapshot state.
    pub fn reset(&mut self) {
        self.context = None;
        self.pending_snapshot = None;
    }
}

/// One validated application update retaining its client subscription identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedMarketBarUpdate {
    pub subscription_id: String,
    pub update: ReplayStreamUpdate,
}

/// Bounded binary fixture/client decoder from transport chunks to application updates.
///
/// This is transport framing and semantic validation only; it does not claim a
/// connected WebSocket, live provider, entitlement service, or production readiness.
#[derive(Clone, Debug, PartialEq)]
pub struct BinaryMarketBarStreamDecoder {
    frames: BoundedBinaryFrameDecoder,
    stream: MarketBarStreamDecoder,
    provenance: ReplayProvenance,
    invalid: bool,
}

impl BinaryMarketBarStreamDecoder {
    /// Creates an explicitly bounded binary stream decoder.
    ///
    /// # Errors
    ///
    /// Returns an error if the framing buffer cannot hold one maximum frame.
    pub fn try_new(
        decimal_convention: DecimalConvention,
        provenance: ReplayProvenance,
        maximum_frame_bytes: NonZeroUsize,
        maximum_buffered_bytes: NonZeroUsize,
    ) -> Result<Self, BinaryMarketStreamError> {
        Ok(Self {
            frames: BoundedBinaryFrameDecoder::try_new(
                maximum_frame_bytes,
                maximum_buffered_bytes,
            )?,
            stream: MarketBarStreamDecoder::new(decimal_convention),
            provenance,
            invalid: false,
        })
    }

    /// Decodes fragmented or coalesced binary frames atomically into application updates.
    ///
    /// Any framing, Protobuf, adapter, or application error latches the decoder invalid.
    /// Call [`Self::reset`] only after reconnect or an explicit resnapshot boundary.
    ///
    /// # Errors
    ///
    /// Returns the first validation failure or a reset-required error after a prior fault.
    pub fn push(
        &mut self,
        chunk: &[u8],
    ) -> Result<Vec<ProjectedMarketBarUpdate>, BinaryMarketStreamError> {
        if self.invalid {
            return Err(BinaryMarketStreamError::ResetRequired);
        }
        let frames = match self.frames.push(chunk) {
            Ok(frames) => frames,
            Err(error) => {
                self.invalid = true;
                return Err(error.into());
            }
        };
        let mut candidate = self.stream.clone();
        let decoded = frames
            .into_iter()
            .try_fold(Vec::new(), |mut updates, frame| {
                let envelope = market_wire::MarketBarStreamEnvelope::decode(frame.as_slice())?;
                if let Some(update) = candidate.try_decode(envelope)? {
                    updates.push(project_market_bar_update(update, self.provenance)?);
                }
                Ok::<_, BinaryMarketStreamError>(updates)
            });
        match decoded {
            Ok(updates) => {
                self.stream = candidate;
                Ok(updates)
            }
            Err(error) => {
                self.invalid = true;
                Err(error)
            }
        }
    }

    /// Clears partial transport and semantic state after explicit reconnect/recovery.
    pub fn reset(&mut self) {
        self.frames.reset();
        self.stream.reset();
        self.invalid = false;
    }

    #[must_use]
    pub const fn is_invalid(&self) -> bool {
        self.invalid
    }

    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.frames.buffered_bytes()
    }
}

fn project_market_bar_update(
    decoded: DecodedMarketBarStreamUpdate,
    provenance: ReplayProvenance,
) -> Result<ProjectedMarketBarUpdate, BinaryMarketStreamError> {
    match decoded {
        DecodedMarketBarStreamUpdate::Snapshot(snapshot) => {
            let DecodedMarketBarSnapshot {
                subscription_id,
                instrument,
                bar_definition,
                stream,
                evidence,
            } = snapshot;
            let replay = ReplaySnapshot::try_new_provenanced(
                instrument,
                provenance,
                bar_definition,
                evidence,
                stream.items().to_vec(),
            )?;
            Ok(ProjectedMarketBarUpdate {
                subscription_id,
                update: ReplayStreamUpdate::Snapshot(replay),
            })
        }
        DecodedMarketBarStreamUpdate::Delta(delta) => {
            validate_provenanced_market_bar(delta.delta.item())?;
            Ok(ProjectedMarketBarUpdate {
                subscription_id: delta.subscription_id,
                update: ReplayStreamUpdate::Delta(delta.delta),
            })
        }
    }
}
