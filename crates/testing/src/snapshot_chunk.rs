//! Bounded atomic multi-frame snapshot assembly evidence.
//!
//! This module owns chunked snapshot framing, ordering and reset behavior, identity
//! mismatch rejection, delta interleaving, and checksum verification for the embedded
//! replay fixture. It reuses the shared binary decoder and check-bit helpers.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, binary_fixture_decoder, passed_check_bits,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error};
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, ReplayStreamUpdate,
};
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, DecimalConvention, ProjectedMarketBarUpdate,
    encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes,
};
use axiusflow_protocols::protobuf::axiusflow::market::v1::market_bar_stream_envelope;
use std::num::NonZeroUsize;

/// Headless evidence for bounded atomic multi-frame snapshot assembly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotChunkConformance {
    passed_checks: u8,
}

impl SnapshotChunkConformance {
    const REQUIRED_CHECKS: u8 = 0b0111_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

struct SnapshotChunkFixture {
    convention: DecimalConvention,
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
    baseline: axiusflow_application::ReplaySnapshot,
    envelopes: Vec<axiusflow_protocols::protobuf::axiusflow::market::v1::MarketBarStreamEnvelope>,
    frames: Vec<Vec<u8>>,
}

/// Exercises bounded chunk order, identity, atomicity, integrity, and reset behavior.
///
/// This proves an in-process fixture protocol only, not a connected WebSocket,
/// provider, entitlement service, or production snapshot source.
///
/// # Errors
///
/// Returns an error when a valid fixture snapshot cannot traverse the chunk pipeline.
pub fn run_snapshot_chunk_conformance() -> Result<SnapshotChunkConformance, ConformanceHarnessError>
{
    let fixture = snapshot_chunk_fixture()?;
    let (atomic_fragmented, frame_bounds) = fragmented_chunk_check(&fixture)?;
    let coalesced = coalesced_chunk_check(&fixture)?;
    let (out_of_order, reset_recovered) = order_and_reset_check(&fixture)?;
    let identity_mismatch = identity_mismatch_check(&fixture)?;
    let delta_interleaving = delta_interleaving_check(&fixture)?;
    let checksum_corruption = chunk_checksum_check(&fixture)?;
    Ok(SnapshotChunkConformance {
        passed_checks: passed_check_bits([
            atomic_fragmented,
            coalesced,
            out_of_order,
            identity_mismatch,
            delta_interleaving,
            checksum_corruption,
            reset_recovered && frame_bounds,
        ]),
    })
}

fn snapshot_chunk_fixture() -> Result<SnapshotChunkFixture, ConformanceHarnessError> {
    let convention =
        DecimalConvention::try_new("usd_minor", "shares").map_err(market_stream_error)?;
    let maximum_frame_bytes = NonZeroUsize::new(65_536).unwrap_or(NonZeroUsize::MIN);
    let maximum_buffered_bytes = NonZeroUsize::new(131_072).unwrap_or(NonZeroUsize::MIN);
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 5 })
        .map_err(market_stream_error)?;
    let envelopes = try_encode_replay_snapshot_chunk_envelopes(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        "fixture_snapshot_chunks",
        &baseline,
        &convention,
        NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(market_stream_error)?;
    let frames = envelopes
        .iter()
        .map(|envelope| encode_market_bar_stream_frame(envelope, maximum_frame_bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(market_stream_error)?;
    Ok(SnapshotChunkFixture {
        convention,
        maximum_frame_bytes,
        maximum_buffered_bytes,
        baseline,
        envelopes,
        frames,
    })
}

fn chunk_decoder(
    fixture: &SnapshotChunkFixture,
) -> Result<BinaryMarketBarStreamDecoder, ConformanceHarnessError> {
    binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )
}

fn fragmented_chunk_check(
    fixture: &SnapshotChunkFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let mut decoder = chunk_decoder(fixture)?;
    let mut projected = Vec::new();
    let mut published_early = false;
    for (frame_index, frame) in fixture.frames.iter().enumerate() {
        for bytes in frame.chunks(11) {
            let updates = decoder.push(bytes).map_err(market_stream_error)?;
            if frame_index + 1 < fixture.frames.len() && !updates.is_empty() {
                published_early = true;
            }
            projected.extend(updates);
        }
    }
    let frame_bounds = fixture.frames.iter().all(|frame| {
        frame.len()
            <= fixture
                .maximum_frame_bytes
                .get()
                .saturating_add(axiusflow_transport::BINARY_FRAME_LENGTH_BYTES)
    });
    Ok((
        !published_early && chunked_snapshot_matches(&projected, fixture),
        frame_bounds,
    ))
}

fn coalesced_chunk_check(fixture: &SnapshotChunkFixture) -> Result<bool, ConformanceHarnessError> {
    let mut bytes = Vec::new();
    for frame in &fixture.frames {
        bytes.extend_from_slice(frame);
    }
    let updates = chunk_decoder(fixture)?
        .push(&bytes)
        .map_err(market_stream_error)?;
    Ok(chunked_snapshot_matches(&updates, fixture))
}

fn order_and_reset_check(
    fixture: &SnapshotChunkFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let second = fixture.frames.get(1).ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing second snapshot chunk".to_string())
    })?;
    let mut decoder = chunk_decoder(fixture)?;
    let out_of_order = decoder.push(second).is_err() && decoder.is_invalid();
    decoder.reset();
    let mut recovered = Vec::new();
    for frame in &fixture.frames {
        recovered.extend(decoder.push(frame).map_err(market_stream_error)?);
    }
    Ok((
        out_of_order,
        chunked_snapshot_matches(&recovered, fixture) && !decoder.is_invalid(),
    ))
}

fn identity_mismatch_check(
    fixture: &SnapshotChunkFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut envelopes = fixture.envelopes.clone();
    let changed = envelopes.get_mut(1).is_some_and(|envelope| {
        let Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) = envelope.update.as_mut()
        else {
            return false;
        };
        snapshot.snapshot_id.push_str("_changed");
        true
    });
    let first = fixture.frames.first().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing first snapshot chunk".to_string())
    })?;
    let changed_frame = envelopes
        .get(1)
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing changed snapshot chunk".to_string())
        })
        .and_then(|envelope| {
            encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes)
                .map_err(market_stream_error)
        })?;
    let mut decoder = chunk_decoder(fixture)?;
    decoder.push(first).map_err(market_stream_error)?;
    Ok(changed && decoder.push(&changed_frame).is_err() && decoder.is_invalid())
}

fn delta_interleaving_check(
    fixture: &SnapshotChunkFixture,
) -> Result<bool, ConformanceHarnessError> {
    let delta = EmbeddedReplaySource
        .load_delta(fixture.baseline.stream().last_sequence())
        .map_err(market_stream_error)?
        .ok_or_else(|| ConformanceHarnessError::MarketStream("missing chunk delta".to_string()))?;
    let envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.baseline.instrument(),
        fixture.baseline.bar_definition(),
        &delta,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let delta_frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let first = fixture.frames.first().ok_or_else(|| {
        ConformanceHarnessError::MarketStream("missing first snapshot chunk".to_string())
    })?;
    let mut decoder = chunk_decoder(fixture)?;
    decoder.push(first).map_err(market_stream_error)?;
    Ok(decoder.push(&delta_frame).is_err() && decoder.is_invalid())
}

fn chunk_checksum_check(fixture: &SnapshotChunkFixture) -> Result<bool, ConformanceHarnessError> {
    let mut envelopes = fixture.envelopes.clone();
    let mut corrupted = true;
    for envelope in &mut envelopes {
        let Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) = envelope.update.as_mut()
        else {
            corrupted = false;
            continue;
        };
        if let Some(first) = snapshot.checksum.first_mut() {
            *first ^= 0xff;
        } else {
            corrupted = false;
        }
    }
    let frames = envelopes
        .iter()
        .map(|envelope| encode_market_bar_stream_frame(envelope, fixture.maximum_frame_bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(market_stream_error)?;
    let mut decoder = chunk_decoder(fixture)?;
    let mut rejected_at_completion = false;
    for (index, frame) in frames.iter().enumerate() {
        if let Ok(updates) = decoder.push(frame) {
            if index + 1 < frames.len() && !updates.is_empty() {
                return Ok(false);
            }
        } else {
            rejected_at_completion = index + 1 == frames.len() && decoder.is_invalid();
            break;
        }
    }
    Ok(corrupted && rejected_at_completion)
}

fn chunked_snapshot_matches(
    updates: &[ProjectedMarketBarUpdate],
    fixture: &SnapshotChunkFixture,
) -> bool {
    updates.len() == 1
        && updates[0].subscription_id == BINARY_FIXTURE_SUBSCRIPTION_ID
        && matches!(
            &updates[0].update,
            ReplayStreamUpdate::Snapshot(snapshot) if snapshot == &fixture.baseline
        )
}
