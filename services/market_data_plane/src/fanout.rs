//! Direct-and-durable fanout wiring for completed bars.
//!
//! Every completed bar is a canonical event accepted by its product's fenced
//! partition and published through the realtime `DirectDurableFanout` contract:
//! the direct branch feeds client streams, the durable branch feeds the
//! Redpanda tap. Ordering, fencing, and overflow behavior live in the realtime
//! contract, not in ad-hoc broadcast code.

use crate::instruments::ProductMapping;
use axiusflow_market_data::MarketBar;
use axiusflow_realtime::LatestStateSnapshotRequest;
use axiusflow_realtime::{
    BoundedEventBranch, CanonicalEventHeader, CanonicalLatestState, CanonicalMarketEvent,
    CanonicalSeriesIdentity, CanonicalSnapshot, CanonicalTimestamps, DirectDurableFanout,
    FencedPartition, OverflowAction, PartitionAcceptance, PartitionDecision, PartitionOwner,
    PublicationFence, QueuePolicy, SemanticClass,
};
use axiusflow_streaming::DurableTopic;
#[cfg(any(test, all(target_os = "linux", feature = "redpanda")))]
use axiusflow_streaming::{DurableEventEnvelope, EventMetadataInput};
#[cfg(any(test, all(target_os = "linux", feature = "redpanda")))]
use serde::Serialize;
use std::num::NonZeroU64;
use std::num::NonZeroUsize;

/// One product's partition and fanout.
pub struct ProductFanout {
    partition: FencedPartition,
    fence: PublicationFence,
    fanout: DirectDurableFanout,
    latest_state: Option<CanonicalLatestState>,
    durable_topic: DurableTopic,
}

/// One fanned-out bar: direct event for clients and durable event for the tap.
pub struct FannedBar {
    pub direct: CanonicalMarketEvent,
    pub durable: CanonicalMarketEvent,
}

#[cfg(any(test, all(target_os = "linux", feature = "redpanda")))]
#[derive(Serialize)]
struct DurableCanonicalEvent<'a> {
    format_version: u32,
    event: &'a CanonicalMarketEvent,
}

fn branch_policy(name: &str) -> Result<QueuePolicy, String> {
    Ok(QueuePolicy {
        name: name.to_string(),
        producer: "market_data_plane".to_string(),
        consumer: format!("axiusflow_{name}_consumer"),
        item_capacity: NonZeroUsize::new(1_024).ok_or("capacity cannot be zero")?,
        byte_capacity: NonZeroUsize::new(1_048_576).ok_or("capacity cannot be zero")?,
        semantic_class: SemanticClass::OrderedDelta,
        overflow_action: OverflowAction::RequestSnapshot,
        maximum_residence_nanos: NonZeroU64::new(100_000_000).ok_or("residence cannot be zero")?,
        recovery: "install_verified_snapshot".to_string(),
        alert_threshold_items: NonZeroUsize::MIN,
    })
}

impl ProductFanout {
    /// Creates the fenced partition and fanout for one product.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid ownership, policy, or topic values.
    pub fn try_new(
        partition_id: u32,
        owner_id: &str,
        environment: &str,
        ownership_epoch: u64,
    ) -> Result<Self, String> {
        let owner = PartitionOwner::try_new(partition_id, owner_id, ownership_epoch)
            .map_err(|error| error.to_string())?;
        let partition = FencedPartition::new(owner);
        let fence = partition.publication_fence();
        let direct = BoundedEventBranch::try_new(branch_policy("direct")?)
            .map_err(|error| error.to_string())?;
        let durable = BoundedEventBranch::try_new(branch_policy("durable")?)
            .map_err(|error| error.to_string())?;
        let fanout = DirectDurableFanout::try_new(partition.publication_fence(), direct, durable)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            partition,
            fence,
            fanout,
            latest_state: None,
            durable_topic: durable_bar_topic(environment)?,
        })
    }

    /// Installs the completed backfill as the initial canonical latest state.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, non-contiguous, or invalid backfill state.
    pub fn install_backfill(
        &mut self,
        mapping: &ProductMapping,
        bars: &[MarketBar],
        now_unix_nanos: i64,
    ) -> Result<(), String> {
        let events = bars
            .iter()
            .map(|bar| self.canonical_event(mapping, bar, now_unix_nanos))
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = CanonicalSnapshot::try_from_events(
            self.fence.partition_id(),
            self.fence.ownership_epoch(),
            1,
            1,
            events,
        )
        .map_err(|error| error.to_string())?;
        let capacity = NonZeroUsize::new(1_024).ok_or("latest-state capacity cannot be zero")?;
        self.latest_state = Some(
            CanonicalLatestState::try_new(&mut self.partition, &self.fence, &snapshot, capacity)
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }

    /// Accepts and publishes one completed bar through the fanout.
    ///
    /// # Errors
    ///
    /// Returns an error for partition rejection, fanout failure, or encoding
    /// failures; these latch snapshot requirements rather than lose silently.
    pub fn publish_bar(
        &mut self,
        mapping: &ProductMapping,
        bar: &MarketBar,
        now_unix_nanos: i64,
    ) -> Result<FannedBar, String> {
        let event = self.canonical_event(mapping, bar, now_unix_nanos)?;
        let accepted = match self
            .partition
            .accept_for_publication(&self.fence, event)
            .map_err(|error| error.to_string())?
        {
            PartitionAcceptance::Accepted(accepted) => *accepted,
            outcome => return Err(format!("bar not accepted for publication: {outcome:?}")),
        };
        self.fanout
            .publish(&mut self.partition, &self.fence, accepted, now_unix_nanos)
            .map_err(|error| error.to_string())?;
        let (direct, _) = self
            .fanout
            .pop_direct(now_unix_nanos)
            .ok_or_else(|| "direct fanout accepted nothing".to_string())?;
        let direct_event = direct.event().clone();
        let latest_state = self
            .latest_state
            .as_mut()
            .ok_or_else(|| "latest state has not been initialized".to_string())?;
        match latest_state
            .apply_direct_event(&self.partition, &self.fence, direct)
            .map_err(|error| error.to_string())?
        {
            PartitionDecision::Accepted => {}
            outcome => return Err(format!("latest state rejected direct event: {outcome:?}")),
        }
        let (durable, _) = self
            .fanout
            .pop_durable(now_unix_nanos)
            .ok_or_else(|| "durable fanout accepted nothing".to_string())?;
        Ok(FannedBar {
            direct: direct_event,
            durable,
        })
    }

    /// Returns the current checksum-verified canonical latest-state tail.
    ///
    /// # Errors
    ///
    /// Returns an error before backfill initialization or while recovery is required.
    pub fn latest_snapshot(
        &self,
        maximum_items: NonZeroUsize,
    ) -> Result<CanonicalSnapshot, String> {
        self.latest_state
            .as_ref()
            .ok_or_else(|| "latest state has not been initialized".to_string())?
            .serve_snapshot(
                &self.partition,
                &self.fence,
                LatestStateSnapshotRequest::new(maximum_items),
            )
            .map_err(|error| error.to_string())
    }

    /// The product's durable topic.
    #[allow(dead_code)]
    #[must_use]
    pub const fn durable_topic(&self) -> &DurableTopic {
        &self.durable_topic
    }

    fn canonical_event(
        &self,
        mapping: &ProductMapping,
        bar: &MarketBar,
        now_unix_nanos: i64,
    ) -> Result<CanonicalMarketEvent, String> {
        let exchange_unix_nanos = bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
        let payload =
            axiusflow_market_protocol_adapter::try_encode_canonical_market_bar_payload(bar)
                .map_err(|error| format!("{error:?}"))?;
        let header = CanonicalEventHeader {
            event_id: format!(
                "coinbase_bar:{}:{}:{}",
                mapping.instrument.instrument_id.as_str(),
                bar.exchange_timestamp_seconds,
                bar.source_sequence
            ),
            event_time_unix_nanos: exchange_unix_nanos,
            publication_time_unix_nanos: now_unix_nanos,
            producer: "market_data_plane".to_string(),
            correlation_id: "coinbase_live_feed".to_string(),
            causation_id: String::new(),
            entitlement_revision: "crypto_public_realtime_v1".to_string(),
            schema_version: 1,
            semantic_class: SemanticClass::OrderedDelta,
            instrument_id: mapping.instrument.instrument_id.as_str().to_string(),
            venue_id: "COINBASE".to_string(),
            source_id: "coinbase".to_string(),
            series_identity: Some(CanonicalSeriesIdentity {
                instrument_revision: mapping.instrument.revision,
                definition_id: mapping.bar_definition.definition_id.clone(),
                definition_version: mapping.bar_definition.version,
                interval_seconds: mapping.bar_definition.interval_seconds,
            }),
            source_sequence: bar.source_sequence,
            partition_id: self.fence.partition_id(),
            ownership_epoch: self.fence.ownership_epoch(),
            timestamps: CanonicalTimestamps {
                exchange_unix_nanos,
                provider_receive_unix_nanos: now_unix_nanos,
                nic_receive_unix_nanos: None,
                axiusflow_receive_unix_nanos: now_unix_nanos,
                normalized_unix_nanos: now_unix_nanos,
                fanout_enqueue_unix_nanos: Some(now_unix_nanos),
            },
            nic_timestamp_source: None,
            correction_flags: 0,
            quality_flags: 0,
        };
        CanonicalMarketEvent::try_new(header, &payload).map_err(|error| error.to_string())
    }
}

/// The plane's Section 11.2 durable bar topic for one deployment environment.
///
/// # Errors
///
/// Returns an error for an invalid environment segment.
pub fn durable_bar_topic(environment: &str) -> Result<DurableTopic, String> {
    DurableTopic::try_new(environment, "market", "bar", "normalized", 1)
        .map_err(|error| error.to_string())
}

/// Builds the Section 11.2 envelope for one durable event.
///
/// # Errors
///
/// Returns an error for invalid metadata or oversized payloads.
#[cfg(any(test, all(target_os = "linux", feature = "redpanda")))]
pub fn durable_envelope(event: &CanonicalMarketEvent) -> Result<DurableEventEnvelope, String> {
    let header = event.header();
    let payload = serde_json::to_vec(&DurableCanonicalEvent {
        format_version: 1,
        event,
    })
    .map_err(|error| format!("cannot encode canonical durable event: {error}"))?;
    DurableEventEnvelope::try_new(
        EventMetadataInput {
            event_id: header.event_id.clone(),
            event_time_unix_nanos: header.event_time_unix_nanos,
            publication_time_unix_nanos: header.publication_time_unix_nanos,
            producer: header.producer.clone(),
            correlation_id: header.correlation_id.clone(),
            causation_id: header.causation_id.clone(),
            partition_id: header.partition_id,
            ownership_epoch: header.ownership_epoch,
        },
        payload,
    )
    .map_err(|error| error.to_string())
}

/// The Section 11.3 partition key for one durable market event: instrument
/// identity plus source.
#[must_use]
#[cfg(any(test, all(target_os = "linux", feature = "redpanda")))]
pub fn durable_partition_key(event: &CanonicalMarketEvent) -> Vec<u8> {
    let header = event.header();
    format!("{}:{}", header.instrument_id, header.source_id).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{ProductFanout, durable_envelope, durable_partition_key};
    use crate::instruments::map_product;
    use axiusflow_market_data::MarketBar;
    use std::num::NonZeroUsize;

    fn bar(source_sequence: u64) -> MarketBar {
        MarketBar {
            source_sequence,
            exchange_timestamp_seconds: 1_754_000_000 + source_sequence.cast_signed() * 60,
            open: 10_000,
            high: 10_150,
            low: 9_900,
            close: 10_120,
            volume: 250_000_000,
        }
    }

    #[test]
    fn completed_bars_flow_through_both_bounded_branches() {
        let mapping = map_product("BTC-USD").expect("BTC-USD maps");
        let mut fanout =
            ProductFanout::try_new(1, "market_data_plane", "test", 1).expect("valid fanout");
        fanout
            .install_backfill(&mapping, &[bar(1)], 1_754_000_000_000_000_000)
            .expect("backfill installs");
        let fanned = fanout
            .publish_bar(&mapping, &bar(2), 1_754_000_060_000_000_000)
            .expect("the next bar is accepted");
        let direct_header = fanned.direct.header();
        assert_eq!(direct_header.source_sequence, 2);
        assert_eq!(direct_header.partition_id, 1);
        assert_eq!(
            direct_header.instrument_id,
            mapping.instrument.instrument_id.as_str()
        );
        let durable_header = fanned.durable.header();
        assert_eq!(
            durable_header.event_id,
            "coinbase_bar:instrument:coinbase:btc:usd:1754000120:2"
        );
        assert_eq!(
            durable_header.event_id,
            fanned.direct.header().event_id,
            "both branches preserve the same event identity"
        );
        let envelope = durable_envelope(&fanned.durable).expect("envelope builds");
        assert_eq!(envelope.metadata().producer, "market_data_plane");
        assert_eq!(envelope.metadata().partition_id, 1);
        assert_eq!(envelope.metadata().ownership_epoch, 1);
        let durable: serde_json::Value =
            serde_json::from_slice(envelope.payload()).expect("durable canonical event decodes");
        assert_eq!(durable["format_version"], 1);
        assert_eq!(durable["event"]["header"]["source_sequence"], 2);
        assert_eq!(
            durable["event"]["header"]["instrument_id"],
            mapping.instrument.instrument_id.as_str()
        );
        assert_eq!(
            durable["event"]["header"]["timestamps"]["fanout_enqueue_unix_nanos"],
            1_754_000_060_000_000_000_i64
        );
        assert_eq!(
            durable["event"]["payload"]
                .as_array()
                .expect("canonical payload bytes")
                .len(),
            axiusflow_market_protocol_adapter::CANONICAL_MARKET_BAR_PAYLOAD_BYTES
        );
        assert_eq!(
            durable_partition_key(&fanned.durable),
            b"instrument:coinbase:btc:usd:coinbase".to_vec()
        );
        assert_eq!(
            fanout.durable_topic().name(),
            "test.market.bar.normalized.v1"
        );
        let latest = fanout
            .latest_snapshot(NonZeroUsize::new(2).expect("nonzero"))
            .expect("latest state serves");
        assert_eq!(latest.events().len(), 2);
        assert_eq!(latest.descriptor().last_sequence.get(), 2);
    }

    #[test]
    fn duplicate_and_gapped_sequences_are_not_republished() {
        let mapping = map_product("ETH-USD").expect("ETH-USD maps");
        let mut fanout =
            ProductFanout::try_new(2, "market_data_plane", "test", 1).expect("valid fanout");
        fanout
            .install_backfill(&mapping, &[bar(1)], 1_754_000_000_000_000_000)
            .expect("backfill installs");
        assert!(
            fanout
                .publish_bar(&mapping, &bar(1), 1_754_000_120_000_000_000)
                .is_err(),
            "a duplicate sequence must not be republished"
        );
        assert!(
            fanout
                .publish_bar(&mapping, &bar(3), 1_754_000_180_000_000_000)
                .is_err(),
            "a gap must latch instead of silently continuing"
        );
        assert!(
            fanout
                .publish_bar(&mapping, &bar(2), 1_754_000_240_000_000_000)
                .is_err(),
            "the latched partition requires snapshot recovery"
        );
    }

    #[test]
    fn product_identity_and_reserved_epoch_are_preserved() {
        let btc_mapping = map_product("BTC-USD").expect("BTC-USD maps");
        let eth_mapping = map_product("ETH-USD").expect("ETH-USD maps");
        let mut btc =
            ProductFanout::try_new(1, "market_data_plane", "test", 41).expect("BTC fanout");
        let mut eth =
            ProductFanout::try_new(2, "market_data_plane", "test", 42).expect("ETH fanout");
        btc.install_backfill(&btc_mapping, &[bar(1)], 1_754_000_000_000_000_000)
            .expect("BTC backfill");
        eth.install_backfill(&eth_mapping, &[bar(1)], 1_754_000_000_000_000_000)
            .expect("ETH backfill");
        let btc_event = btc
            .publish_bar(&btc_mapping, &bar(2), 1_754_000_060_000_000_000)
            .expect("BTC event")
            .durable;
        let eth_event = eth
            .publish_bar(&eth_mapping, &bar(2), 1_754_000_060_000_000_000)
            .expect("ETH event")
            .durable;
        assert_ne!(btc_event.header().event_id, eth_event.header().event_id);
        assert_eq!(btc_event.header().ownership_epoch, 41);
        assert_eq!(eth_event.header().ownership_epoch, 42);
    }

    #[test]
    fn invalid_owners_and_environments_fail_closed() {
        assert!(ProductFanout::try_new(1, "", "test", 1).is_err());
        assert!(ProductFanout::try_new(1, "market_data_plane", "Prod", 1).is_err());
        assert!(ProductFanout::try_new(1, "market_data_plane", "test", 0).is_err());
    }
}
