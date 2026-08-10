use super::{SCHEMA_VERSION, unix_nanos};
use axiusflow_application::{
    MarketEventProvenance, Provenanced, ProvenancedMarketBar, validate_provenanced_market_bar,
};
use axiusflow_coinbase_market_adapter::{CoinbaseAggregatedBar, ENTITLEMENT_CLASS};
use axiusflow_desktop_provider_runtime::SessionGeneration;
use axiusflow_market_data::MarketBar;

struct ProvenanceContext {
    generation: u64,
    session_generation: u64,
    event_id: String,
    event_time_unix_nanos: i64,
    provider_receive_timestamp_unix_nanos: i64,
    received_unix_nanos: i64,
    causation_id: String,
}

pub(super) fn history_provenance(
    bar: MarketBar,
    generation: SessionGeneration,
    received_unix_nanos: i64,
) -> Result<ProvenancedMarketBar, String> {
    let exchange = bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase history timestamp overflow".to_string())?;
    provenanced(
        bar,
        ProvenanceContext {
            generation: generation.get(),
            session_generation: generation.get().saturating_add(1),
            event_id: format!("coinbase_history_bar_{}_{exchange}", bar.source_sequence),
            event_time_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: received_unix_nanos,
            received_unix_nanos,
            causation_id: "coinbase_https_history".to_string(),
        },
    )
}

pub(super) fn cached_history_provenance(
    bar: MarketBar,
    cache_generation: u64,
    received_unix_nanos: i64,
) -> Result<ProvenancedMarketBar, String> {
    let exchange = bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase cached history timestamp overflow".to_string())?;
    provenanced(
        bar,
        ProvenanceContext {
            generation: cache_generation,
            session_generation: 1,
            event_id: format!(
                "coinbase_cached_history_bar_{}_{exchange}",
                bar.source_sequence
            ),
            event_time_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: received_unix_nanos,
            received_unix_nanos,
            causation_id: "coinbase_encrypted_local_history".to_string(),
        },
    )
}

pub(super) fn live_provenance(
    completed: CoinbaseAggregatedBar,
    generation: SessionGeneration,
) -> Result<ProvenancedMarketBar, String> {
    let provider_sequence_num = completed
        .provider_sequence_num
        .ok_or_else(|| "Coinbase completed bar has no live provider sequence".to_string())?;
    let provider_timestamp_unix_nanos = completed
        .provider_timestamp_unix_nanos
        .ok_or_else(|| "Coinbase completed bar has no live provider timestamp".to_string())?;
    let received = unix_nanos()?;
    let exchange = completed
        .bar
        .exchange_timestamp_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase live bar timestamp overflow".to_string())?;
    provenanced(
        completed.bar,
        ProvenanceContext {
            generation: generation.get(),
            session_generation: generation.get().saturating_add(1),
            event_id: format!(
                "coinbase_live_bar_{}_message_{}",
                completed.bar.source_sequence, provider_sequence_num
            ),
            event_time_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: provider_timestamp_unix_nanos,
            received_unix_nanos: received,
            causation_id: format!("coinbase_message_sequence_{provider_sequence_num}"),
        },
    )
}

fn provenanced(bar: MarketBar, context: ProvenanceContext) -> Result<ProvenancedMarketBar, String> {
    let exchange_timestamp_unix_nanos =
        bar.exchange_timestamp_seconds
            .checked_mul(1_000_000_000)
            .ok_or_else(|| "Coinbase bar timestamp overflow".to_string())?;
    let item = Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id: context.event_id,
            event_time_unix_nanos: context.event_time_unix_nanos,
            publication_time_unix_nanos: context.received_unix_nanos,
            producer: "axiusflow_desktop_coinbase_worker".to_string(),
            schema_version: SCHEMA_VERSION,
            correlation_id: format!("coinbase_generation_{}", context.generation),
            causation_id: context.causation_id,
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            session_generation: context.session_generation,
            source_id: "coinbase".to_string(),
            source_sequence: bar.source_sequence,
            exchange_timestamp_unix_nanos,
            provider_receive_timestamp_unix_nanos: context.provider_receive_timestamp_unix_nanos,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: context.received_unix_nanos,
            normalized_timestamp_unix_nanos: context.received_unix_nanos,
            fanout_enqueue_timestamp_unix_nanos: None,
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    );
    validate_provenanced_market_bar(&item).map_err(|error| error.to_string())?;
    Ok(item)
}
