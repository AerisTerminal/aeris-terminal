//! Request context and provenance-bearing market values.

use crate::errors::ReplayValidationError;
use crate::stream::{
    MarketEventProvenance, MarketSnapshotIdentityRef, MarketValueChecksumRef, Provenanced,
    SnapshotEvidence, compute_market_snapshot_checksum,
};
use aeris_instruments::InstrumentRevision;
use aeris_market_data::{BarDefinition, MarketBar};

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
    /// A live authorized provider stream; the venue is recorded per event.
    LiveProvider,
}

impl ReplayProvenance {
    /// Returns a user-facing source label that does not imply a live connection.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::EmbeddedFixture => "embedded replay fixture",
            Self::LiveProvider => "live authorized provider",
        }
    }
}

/// One fixed-point bar inseparable from source, session, quality, and entitlement evidence.
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
    if provenance.exchange_timestamp_unix_nanos != bar.exchange_timestamp_unix_nanos {
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
    if provenance.session_generation == 0 || provenance.schema_version == 0 {
        return Err(ReplayValidationError::InvalidProvenanceRevision);
    }
    Ok(())
}

pub(crate) fn try_provenanced_market_bar(
    bar: MarketBar,
    provenance: MarketEventProvenance,
) -> Result<ProvenancedMarketBar, ReplayValidationError> {
    let item = Provenanced::new(bar, provenance);
    validate_provenanced_market_bar(&item)?;
    Ok(item)
}

pub(crate) fn snapshot_checksum(
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
            bar_trades_per_bar: bar_definition.trades_per_bar,
            bar_calendar_months: bar_definition.calendar_months,
        },
        bars.iter().map(|item| {
            let bar = item.value();
            MarketValueChecksumRef {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
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

pub(crate) fn embedded_event_provenance(bar: &MarketBar) -> MarketEventProvenance {
    let exchange_timestamp_unix_nanos = bar.exchange_timestamp_unix_nanos;
    MarketEventProvenance {
        event_id: format!("embedded_market_bar_{}", bar.source_sequence),
        event_time_unix_nanos: exchange_timestamp_unix_nanos,
        publication_time_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(4_000),
        producer: "embedded_replay_source".to_string(),
        schema_version: 1,
        correlation_id: "embedded_replay".to_string(),
        causation_id: String::new(),
        entitlement_revision: "embedded_fixture_entitlement_v1".to_string(),
        session_generation: 1,
        source_id: "embedded_fixture".to_string(),
        source_sequence: bar.source_sequence,
        exchange_timestamp_unix_nanos,
        provider_receive_timestamp_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(1_000),
        nic_receive_timestamp_unix_nanos: None,
        aeris_receive_timestamp_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(2_000),
        normalized_timestamp_unix_nanos: exchange_timestamp_unix_nanos.saturating_add(3_000),
        fanout_enqueue_timestamp_unix_nanos: None,
        correction_flags: 0,
        quality_flags: 0,
        nic_timestamp_source: 0,
        semantic_class: 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bar_requires_exact_exchange_provenance() {
        let bar = MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 10,
            exchange_timestamp_unix_nanos: 10_123_456_000,
            open: 100,
            high: 100,
            low: 100,
            close: 100,
            volume: 1,
        };
        let mut provenance = embedded_event_provenance(&bar);
        let item = Provenanced::new(bar, provenance.clone());
        assert!(validate_provenanced_market_bar(&item).is_ok());

        provenance.exchange_timestamp_unix_nanos += 1;
        let mismatched = Provenanced::new(bar, provenance);
        assert!(matches!(
            validate_provenanced_market_bar(&mismatched),
            Err(ReplayValidationError::ProvenanceExchangeTimestampMismatch { .. })
        ));
    }
}
