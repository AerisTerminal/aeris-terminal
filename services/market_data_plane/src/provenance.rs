//! Canonical provenance for Coinbase-sourced bars.

use axiusflow_market_data::MarketBar;
use axiusflow_protocols::MarketEventProvenance;

/// Builds the canonical evidence for one aggregated bar.
///
/// The event identity retains the source sequence; timestamps record the bar's
/// exchange minute and the plane's own receive/normalize instants. The
/// entitlement class `crypto_public_realtime` is recorded as the entitlement
/// revision on every event.
#[must_use]
pub fn coinbase_bar_provenance(
    bar: &MarketBar,
    received_unix_nanos: i64,
    ownership_epoch: u64,
) -> MarketEventProvenance {
    let exchange_timestamp_unix_nanos =
        bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
    MarketEventProvenance {
        event_id: format!("coinbase_market_bar_{}", bar.source_sequence),
        event_time_unix_nanos: exchange_timestamp_unix_nanos,
        publication_time_unix_nanos: received_unix_nanos,
        producer: "coinbase_advanced_trade".to_string(),
        schema_version: 1,
        correlation_id: "coinbase_live_feed".to_string(),
        causation_id: String::new(),
        entitlement_revision: "crypto_public_realtime_v1".to_string(),
        partition_id: 0,
        ownership_epoch,
        source_id: "coinbase".to_string(),
        source_sequence: bar.source_sequence,
        exchange_timestamp_unix_nanos,
        provider_receive_timestamp_unix_nanos: received_unix_nanos,
        nic_receive_timestamp_unix_nanos: None,
        axiusflow_receive_timestamp_unix_nanos: received_unix_nanos,
        normalized_timestamp_unix_nanos: received_unix_nanos,
        fanout_enqueue_timestamp_unix_nanos: None,
        correction_flags: 0,
        quality_flags: 0,
        nic_timestamp_source: 0,
        semantic_class: 2,
    }
}
