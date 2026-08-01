//! Shared binary market-bar stream fixture and decoder helpers.
//!
//! This module owns the deterministic embedded replay fixture used by every binary,
//! WebSocket, snapshot-chunk, and client-model scenario: its encoded frames, its
//! corrupted variants, the bounded decoder construction, and the small check-bit and
//! generation helpers those scenarios share. It contains no scenario assertions.

use crate::harness_error::{ConformanceHarnessError, market_stream_error};
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarReplayPort, ReplayProvenance,
};
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, DecimalConvention, encode_market_bar_stream_frame,
    try_encode_replay_delta_envelope, try_encode_replay_snapshot_envelope,
};
use axiusflow_protocols::protobuf::axiusflow::market::v1::market_bar_stream_envelope;
use std::num::NonZeroUsize;

pub(crate) type FixtureMarketGeneration =
    axiusflow_application::MarketGeneration<axiusflow_application::ProvenancedMarketBar>;

pub(crate) const BINARY_FIXTURE_SUBSCRIPTION_ID: &str = "fixture_market_bars";

pub(crate) struct BinaryMarketStreamFixture {
    pub(crate) convention: DecimalConvention,
    pub(crate) maximum_frame_bytes: NonZeroUsize,
    pub(crate) maximum_buffered_bytes: NonZeroUsize,
    pub(crate) baseline: axiusflow_application::ReplaySnapshot,
    pub(crate) delta: axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
    pub(crate) snapshot_frame: Vec<u8>,
    pub(crate) joined_frames: Vec<u8>,
    pub(crate) corrupted_frame: Vec<u8>,
    pub(crate) checksum_corrupted: bool,
}

pub(crate) fn binary_market_stream_fixture()
-> Result<BinaryMarketStreamFixture, ConformanceHarnessError> {
    let maximum_frame_bytes = NonZeroUsize::new(65_536).unwrap_or(NonZeroUsize::MIN);
    let maximum_buffered_bytes = NonZeroUsize::new(131_072).unwrap_or(NonZeroUsize::MIN);
    let convention =
        DecimalConvention::try_new("usd_minor", "shares").map_err(market_stream_error)?;
    let source = EmbeddedReplaySource;
    let baseline = source
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .map_err(market_stream_error)?;
    let delta = source
        .load_delta(2)
        .map_err(market_stream_error)?
        .ok_or_else(|| {
            ConformanceHarnessError::MarketStream("missing fixture delta".to_string())
        })?;
    let snapshot_envelope =
        try_encode_replay_snapshot_envelope(BINARY_FIXTURE_SUBSCRIPTION_ID, &baseline, &convention)
            .map_err(market_stream_error)?;
    let delta_envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        baseline.instrument(),
        baseline.bar_definition(),
        &delta,
        &convention,
    )
    .map_err(market_stream_error)?;
    let snapshot_frame = encode_market_bar_stream_frame(&snapshot_envelope, maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let delta_frame = encode_market_bar_stream_frame(&delta_envelope, maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let mut joined_frames = snapshot_frame.clone();
    joined_frames.extend_from_slice(&delta_frame);

    let mut corrupted_envelope = snapshot_envelope;
    let checksum_corrupted = corrupt_snapshot_checksum(&mut corrupted_envelope);
    let corrupted_frame = encode_market_bar_stream_frame(&corrupted_envelope, maximum_frame_bytes)
        .map_err(market_stream_error)?;
    Ok(BinaryMarketStreamFixture {
        convention,
        maximum_frame_bytes,
        maximum_buffered_bytes,
        baseline,
        delta,
        snapshot_frame,
        joined_frames,
        corrupted_frame,
        checksum_corrupted,
    })
}

pub(crate) fn corrupt_snapshot_checksum(
    envelope: &mut axiusflow_protocols::protobuf::axiusflow::market::v1::MarketBarStreamEnvelope,
) -> bool {
    match envelope.update.as_mut() {
        Some(market_bar_stream_envelope::Update::Snapshot(snapshot)) => {
            snapshot.checksum.first_mut().is_some_and(|first| {
                *first ^= 0xff;
                true
            })
        }
        Some(market_bar_stream_envelope::Update::Delta(_)) | None => false,
    }
}

pub(crate) fn passed_check_bits(checks: [bool; 7]) -> u8 {
    let mut passed_checks = 0_u8;
    for (bit, passed) in [
        0b0000_0001,
        0b0000_0010,
        0b0000_0100,
        0b0000_1000,
        0b0001_0000,
        0b0010_0000,
        0b0100_0000,
    ]
    .into_iter()
    .zip(checks)
    {
        if passed {
            passed_checks |= bit;
        }
    }
    passed_checks
}

pub(crate) fn binary_fixture_decoder(
    convention: DecimalConvention,
    maximum_frame_bytes: NonZeroUsize,
    maximum_buffered_bytes: NonZeroUsize,
) -> Result<BinaryMarketBarStreamDecoder, ConformanceHarnessError> {
    BinaryMarketBarStreamDecoder::try_new(
        convention,
        ReplayProvenance::EmbeddedFixture,
        maximum_frame_bytes,
        maximum_buffered_bytes,
    )
    .map_err(market_stream_error)
}

pub(crate) fn expect_published_generation(
    outcome: axiusflow_application::MarketBarModelOutcome,
    scenario: &str,
) -> Result<FixtureMarketGeneration, ConformanceHarnessError> {
    if let axiusflow_application::MarketBarModelOutcome::Published(generation) = outcome {
        return Ok(generation);
    }
    Err(ConformanceHarnessError::MarketStream(format!(
        "{scenario} did not publish an immutable generation"
    )))
}

pub(crate) fn replay_snapshot_with_generation(
    snapshot: &axiusflow_application::ReplaySnapshot,
    generation: u64,
) -> Result<axiusflow_application::ReplaySnapshot, ConformanceHarnessError> {
    let mut evidence = snapshot.evidence().clone();
    evidence.generation = generation;
    evidence.checksum = axiusflow_protocols::compute_market_snapshot_checksum(
        &evidence,
        axiusflow_protocols::MarketSnapshotIdentityRef {
            instrument_id: snapshot.instrument().instrument_id.as_str(),
            instrument_revision: snapshot.instrument().revision,
            bar_definition_id: &snapshot.bar_definition().definition_id,
            bar_definition_version: snapshot.bar_definition().version,
            bar_interval_seconds: snapshot.bar_definition().interval_seconds,
        },
        snapshot.bars().iter().map(|item| {
            let bar = item.value();
            axiusflow_protocols::MarketValueChecksumRef {
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
    );
    axiusflow_application::ReplaySnapshot::try_new_provenanced(
        snapshot.instrument().clone(),
        snapshot.provenance(),
        snapshot.bar_definition().clone(),
        evidence,
        snapshot.bars().to_vec(),
    )
    .map_err(market_stream_error)
}
