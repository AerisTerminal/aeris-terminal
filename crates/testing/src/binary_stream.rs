//! Fixture-qualified binary market subscription pipeline evidence.
//!
//! This module owns binary framing checks, malformed and reconnect classification,
//! checksum corruption rejection, sequence-gap latching, and oversized-frame rejection
//! for the shared binary market-bar stream fixture.

use crate::binary_fixture::{
    BINARY_FIXTURE_SUBSCRIPTION_ID, BinaryMarketStreamFixture, binary_fixture_decoder,
    binary_market_stream_fixture, passed_check_bits,
};
use crate::harness_error::{ConformanceHarnessError, market_stream_error, websocket_error};
use crate::loopback_fixture::{websocket_fixture_config, websocket_fixture_session};
use axiusflow_application::{EmbeddedReplaySource, MarketBarReplayPort, ReplayStreamUpdate};
use axiusflow_market_protocol_adapter::{
    ProjectedMarketBarUpdate, encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
};
use axiusflow_stream_websocket_adapter::{MarketWebSocketState, WebSocketRecoveryReason};
use axiusflow_transport::encode_binary_frame;
use tungstenite::Message;

/// Headless evidence for the fixture-qualified binary market subscription pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BinaryMarketStreamConformance {
    passed_checks: u8,
}

impl BinaryMarketStreamConformance {
    const REQUIRED_CHECKS: u8 = 0b0111_1111;

    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.passed_checks == Self::REQUIRED_CHECKS
    }
}

/// Exercises bounded framing, Protobuf projection, reconnect, corruption, and gap behavior.
///
/// This proves a deterministic software stream contract only. It does not prove a
/// live WebSocket, provider connection, entitlement service, or production readiness.
///
/// # Errors
///
/// Returns an error when the embedded fixture or binary pipeline rejects a valid scenario.
pub fn run_binary_market_stream_conformance()
-> Result<BinaryMarketStreamConformance, ConformanceHarnessError> {
    let fixture = binary_market_stream_fixture()?;
    let (fragmented_equivalent, coalesced_equivalent) = framing_checks(&fixture)?;
    let (malformed_rejected, reconnect_restored) = malformed_and_reconnect_checks(&fixture)?;
    let checksum_corruption_rejected = checksum_corruption_check(&fixture)?;
    let gap_rejected = gap_check(&fixture)?;
    let schema_change_rejected = decoder_schema_change_preserves_generation(&fixture)?;
    let oversized_rejected = oversized_frame_check(&fixture)?;
    let passed_checks = passed_check_bits([
        fragmented_equivalent,
        coalesced_equivalent,
        malformed_rejected,
        reconnect_restored,
        checksum_corruption_rejected,
        gap_rejected && schema_change_rejected,
        oversized_rejected,
    ]);
    Ok(BinaryMarketStreamConformance { passed_checks })
}

fn decoder_schema_change_preserves_generation(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut session = websocket_fixture_session(fixture, websocket_fixture_config(fixture, 4)?)?;
    session
        .accept_message(Message::binary(fixture.snapshot_frame.clone()))
        .map_err(websocket_error)?;
    let installed = session.current_generation().cloned();
    let mut provenance = fixture.delta.item().provenance().clone();
    provenance.schema_version = provenance.schema_version.saturating_add(1);
    let changed_item =
        axiusflow_application::Provenanced::new(*fixture.delta.item().value(), provenance);
    let changed_delta = axiusflow_application::StreamDelta::try_new(
        fixture.delta.previous_sequence(),
        fixture.delta.sequence(),
        changed_item,
    )
    .map_err(market_stream_error)?;
    let envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.baseline.instrument(),
        fixture.baseline.bar_definition(),
        &changed_delta,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let frame = encode_market_bar_stream_frame(&envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    Ok(session.accept_message(Message::binary(frame)).is_err()
        && session.current_generation() == installed.as_ref()
        && session.state()
            == MarketWebSocketState::RecoveryRequired(WebSocketRecoveryReason::DecodeFailure))
}

fn framing_checks(
    fixture: &BinaryMarketStreamFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let mut fragmented_decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let mut fragmented_updates = Vec::new();
    let mut observed_partial_frame = false;
    for chunk in fixture.joined_frames.chunks(7) {
        let updates = fragmented_decoder
            .push(chunk)
            .map_err(market_stream_error)?;
        observed_partial_frame |= updates.is_empty() && fragmented_decoder.buffered_bytes() > 0;
        fragmented_updates.extend(updates);
    }
    let fragmented_equivalent = observed_partial_frame
        && projected_updates_match(
            &fragmented_updates,
            BINARY_FIXTURE_SUBSCRIPTION_ID,
            &fixture.baseline,
            &fixture.delta,
        );

    let mut coalesced_decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    let coalesced_updates = coalesced_decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    let coalesced_equivalent = projected_updates_match(
        &coalesced_updates,
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        &fixture.baseline,
        &fixture.delta,
    );
    Ok((fragmented_equivalent, coalesced_equivalent))
}

fn malformed_and_reconnect_checks(
    fixture: &BinaryMarketStreamFixture,
) -> Result<(bool, bool), ConformanceHarnessError> {
    let malformed_payload =
        encode_binary_frame(&[0xff], fixture.maximum_frame_bytes).map_err(market_stream_error)?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    decoder
        .push(&fixture.joined_frames)
        .map_err(market_stream_error)?;
    let malformed_rejected = decoder.push(&malformed_payload).is_err()
        && decoder.is_invalid()
        && decoder.push(&fixture.snapshot_frame).is_err();
    decoder.reset();
    let reconnect_restored = decoder
        .push(&fixture.snapshot_frame)
        .is_ok_and(|updates| updates.len() == 1 && !decoder.is_invalid());
    Ok((malformed_rejected, reconnect_restored))
}

fn checksum_corruption_check(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    Ok(fixture.checksum_corrupted
        && decoder.push(&fixture.corrupted_frame).is_err()
        && decoder.is_invalid())
}

fn gap_check(fixture: &BinaryMarketStreamFixture) -> Result<bool, ConformanceHarnessError> {
    let gap_delta = EmbeddedReplaySource
        .load_delta(3)
        .map_err(market_stream_error)?
        .ok_or_else(|| ConformanceHarnessError::MarketStream("missing gap delta".to_string()))?;
    let gap_envelope = try_encode_replay_delta_envelope(
        BINARY_FIXTURE_SUBSCRIPTION_ID,
        fixture.baseline.instrument(),
        fixture.baseline.bar_definition(),
        &gap_delta,
        &fixture.convention,
    )
    .map_err(market_stream_error)?;
    let gap_frame = encode_market_bar_stream_frame(&gap_envelope, fixture.maximum_frame_bytes)
        .map_err(market_stream_error)?;
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    decoder
        .push(&fixture.snapshot_frame)
        .map_err(market_stream_error)?;
    Ok(decoder.push(&gap_frame).is_err() && decoder.is_invalid())
}

fn oversized_frame_check(
    fixture: &BinaryMarketStreamFixture,
) -> Result<bool, ConformanceHarnessError> {
    let oversized_prefix = u32::try_from(fixture.maximum_frame_bytes.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1)
        .to_be_bytes();
    let mut decoder = binary_fixture_decoder(
        fixture.convention.clone(),
        fixture.maximum_frame_bytes,
        fixture.maximum_buffered_bytes,
    )?;
    Ok(decoder.push(&oversized_prefix).is_err()
        && decoder.is_invalid()
        && decoder.buffered_bytes() == 0)
}

fn projected_updates_match(
    updates: &[ProjectedMarketBarUpdate],
    subscription_id: &str,
    baseline: &axiusflow_application::ReplaySnapshot,
    delta: &axiusflow_protocols::StreamDelta<axiusflow_application::ProvenancedMarketBar>,
) -> bool {
    updates.len() == 2
        && updates
            .iter()
            .all(|update| update.subscription_id == subscription_id)
        && matches!(
            &updates[0].update,
            ReplayStreamUpdate::Snapshot(snapshot) if snapshot == baseline
        )
        && matches!(
            &updates[1].update,
            ReplayStreamUpdate::Delta(actual) if actual == delta
        )
}
