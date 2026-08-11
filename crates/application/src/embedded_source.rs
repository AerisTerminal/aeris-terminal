//! Deterministic embedded replay source behind the replay port.

use crate::errors::ReplayValidationError;
use crate::provenance::{
    ProvenancedMarketBar, ReplayProvenance, embedded_event_provenance, try_provenanced_market_bar,
};
use crate::replay_snapshot::ReplaySnapshot;
use crate::stream::{MAX_STREAM_SNAPSHOT_ITEMS, StreamDelta, StreamProtocolError};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{BarDefinition, MarketBar};

/// Maximum bars accepted by the embedded replay boundary.
pub const MAX_EMBEDDED_REPLAY_BARS: usize = MAX_STREAM_SNAPSHOT_ITEMS;

const EMBEDDED_REPLAY_START_SECONDS: i64 = 1_735_689_600;
const EMBEDDED_REPLAY_INTERVAL_SECONDS: i64 = 60;

/// Request for a bounded deterministic replay snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadEmbeddedReplay {
    pub bar_count: usize,
}

/// Local Stage 1 source used until the real replay transport is implemented.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EmbeddedReplaySource;

impl EmbeddedReplaySource {
    /// Loads a bounded deterministic snapshot.
    ///
    /// # Errors
    /// Returns an error when the requested size or generated replay is invalid.
    pub fn load_snapshot(
        self,
        request: LoadEmbeddedReplay,
    ) -> Result<ReplaySnapshot, ReplayValidationError> {
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

    /// Loads the next deterministic delta, if one remains.
    ///
    /// # Errors
    /// Returns an error when the sequence overflows or the generated delta is invalid.
    pub fn load_delta(
        self,
        previous_sequence: u64,
    ) -> Result<Option<StreamDelta<ProvenancedMarketBar>>, ReplayValidationError> {
        let sequence = previous_sequence
            .checked_add(1)
            .ok_or(StreamProtocolError::SequenceOverflow)?;
        let bar_count = usize::try_from(sequence)
            .map_err(|_| StreamProtocolError::ItemCountOverflow(usize::MAX))?;
        if bar_count > MAX_EMBEDDED_REPLAY_BARS {
            return Ok(None);
        }
        let Some(bar) = embedded_bars(bar_count).pop() else {
            return Ok(None);
        };
        let item = try_provenanced_market_bar(bar, embedded_event_provenance(&bar))?;
        Ok(Some(StreamDelta::try_new(
            previous_sequence,
            sequence,
            item,
        )?))
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
        trades_per_bar: None,
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
            exchange_timestamp_unix_nanos: (EMBEDDED_REPLAY_START_SECONDS
                + index_i64 * EMBEDDED_REPLAY_INTERVAL_SECONDS)
                * 1_000_000_000,
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
