use axiusflow_application::{
    MarketEventProvenance, Provenanced, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{BarDefinition, MarketBar, MarketTrade};
use std::{fmt, num::NonZeroUsize};

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const MAXIMUM_LIVE_CHART_BARS: usize = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RithmicChartGeneration {
    pub(crate) selection: NonZeroUsize,
    pub(crate) series: NonZeroUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RithmicLiveChartUpdate {
    ReplaceForming {
        forming: MarketBar,
    },
    CompleteAndStart {
        completed: MarketBar,
        forming: MarketBar,
    },
}

impl RithmicLiveChartUpdate {
    pub(crate) const fn forming(self) -> MarketBar {
        match self {
            Self::ReplaceForming { forming } | Self::CompleteAndStart { forming, .. } => forming,
        }
    }

    pub(crate) const fn completed(self) -> Option<MarketBar> {
        match self {
            Self::ReplaceForming { .. } => None,
            Self::CompleteAndStart { completed, .. } => Some(completed),
        }
    }

    #[cfg(test)]
    pub(crate) const fn bar_count(self) -> usize {
        match self {
            Self::ReplaceForming { .. } => 1,
            Self::CompleteAndStart { .. } => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RithmicLiveChartError {
    EmptyHistory,
    InvalidTrade,
    StaleGeneration,
    IdentityMismatch,
    MissingExchangeTimestamp,
    OutOfOrderTrade,
    SessionGenerationChanged,
    TimestampOverflow,
    SequenceOverflow,
    VolumeOverflow,
    SnapshotGenerationOverflow,
    SnapshotInvalid,
}

impl fmt::Display for RithmicLiveChartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyHistory => "Rithmic live chart requires a history seed",
            Self::InvalidTrade => "Rithmic live chart received an invalid trade",
            Self::StaleGeneration => "Rithmic live chart rejected a stale selection or series",
            Self::IdentityMismatch => "Rithmic live chart trade identity does not match its seed",
            Self::MissingExchangeTimestamp => "Rithmic live chart trade has no exchange timestamp",
            Self::OutOfOrderTrade => "Rithmic live chart rejected an out-of-order trade",
            Self::SessionGenerationChanged => {
                "Rithmic live chart requires a new snapshot after a session change"
            }
            Self::TimestampOverflow => "Rithmic live chart timestamp overflowed",
            Self::SequenceOverflow => "Rithmic live chart bar sequence overflowed",
            Self::VolumeOverflow => "Rithmic live chart bar volume overflowed",
            Self::SnapshotGenerationOverflow => "Rithmic live chart snapshot generation overflowed",
            Self::SnapshotInvalid => "Rithmic live chart snapshot validation failed",
        })
    }
}

impl std::error::Error for RithmicLiveChartError {}

pub(crate) struct RithmicLiveChart {
    generation: RithmicChartGeneration,
    instrument: InstrumentRevision,
    replay_provenance: ReplayProvenance,
    bar_definition: BarDefinition,
    provider_id: String,
    instrument_id: String,
    entitlement_id: String,
    interval_seconds: i64,
    forming: MarketBar,
    bars: Vec<ProvenancedMarketBar>,
    retained_bar_count: usize,
    snapshot_generation: u64,
    partition_id: u32,
    ownership_epoch: u64,
    schema_version: u32,
    live_session_generation: Option<u64>,
    last_trade_sequence: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RithmicLiveChartPublication {
    pub(crate) generation: RithmicChartGeneration,
    pub(crate) update: RithmicLiveChartUpdate,
    pub(crate) snapshot: ReplaySnapshot,
}

impl RithmicLiveChart {
    pub(crate) fn from_history(
        generation: RithmicChartGeneration,
        snapshot: &ReplaySnapshot,
    ) -> Result<Self, RithmicLiveChartError> {
        let seed = snapshot
            .bars()
            .last()
            .ok_or(RithmicLiveChartError::EmptyHistory)?;
        let first = snapshot
            .bars()
            .first()
            .ok_or(RithmicLiveChartError::EmptyHistory)?;
        let retained_bar_count = snapshot.bars().len();
        if retained_bar_count > MAXIMUM_LIVE_CHART_BARS {
            return Err(RithmicLiveChartError::SnapshotInvalid);
        }
        Ok(Self {
            generation,
            instrument: snapshot.instrument().clone(),
            replay_provenance: snapshot.provenance(),
            bar_definition: snapshot.bar_definition().clone(),
            provider_id: seed.provenance().source_id.clone(),
            instrument_id: snapshot.instrument().instrument_id.as_str().to_string(),
            entitlement_id: seed.provenance().entitlement_revision.clone(),
            interval_seconds: i64::from(snapshot.bar_definition().interval_seconds),
            forming: *seed.value(),
            bars: snapshot.bars().to_vec(),
            retained_bar_count,
            snapshot_generation: snapshot.evidence().generation,
            partition_id: first.provenance().partition_id,
            ownership_epoch: first.provenance().ownership_epoch,
            schema_version: first.provenance().schema_version,
            live_session_generation: None,
            last_trade_sequence: None,
        })
    }

    pub(crate) const fn generation(&self) -> RithmicChartGeneration {
        self.generation
    }

    #[cfg(test)]
    pub(crate) const fn forming(&self) -> MarketBar {
        self.forming
    }

    pub(crate) fn apply_trade(
        &mut self,
        generation: RithmicChartGeneration,
        trade: &MarketTrade,
    ) -> Result<RithmicLiveChartPublication, RithmicLiveChartError> {
        if generation != self.generation {
            return Err(RithmicLiveChartError::StaleGeneration);
        }
        trade
            .validate()
            .map_err(|_| RithmicLiveChartError::InvalidTrade)?;
        if trade.metadata.provider_id != self.provider_id
            || trade.metadata.instrument_id != self.instrument_id
            || trade.metadata.entitlement_id != self.entitlement_id
        {
            return Err(RithmicLiveChartError::IdentityMismatch);
        }
        self.validate_ordering(trade)?;

        let exchange_nanos = trade
            .metadata
            .timestamps
            .exchange_unix_nanos
            .ok_or(RithmicLiveChartError::MissingExchangeTimestamp)?;
        let exchange_seconds = exchange_nanos.div_euclid(NANOS_PER_SECOND);
        let bucket = exchange_seconds
            .div_euclid(self.interval_seconds)
            .checked_mul(self.interval_seconds)
            .ok_or(RithmicLiveChartError::TimestampOverflow)?;
        if bucket < self.forming.exchange_timestamp_seconds {
            return Err(RithmicLiveChartError::OutOfOrderTrade);
        }

        let update = if bucket == self.forming.exchange_timestamp_seconds {
            let mut forming = self.forming;
            forming.high = forming.high.max(trade.price);
            forming.low = forming.low.min(trade.price);
            forming.close = trade.price;
            forming.volume = forming
                .volume
                .checked_add(trade.quantity)
                .ok_or(RithmicLiveChartError::VolumeOverflow)?;
            RithmicLiveChartUpdate::ReplaceForming { forming }
        } else {
            let completed = self.forming;
            let forming = MarketBar {
                source_sequence: completed
                    .source_sequence
                    .checked_add(1)
                    .ok_or(RithmicLiveChartError::SequenceOverflow)?,
                exchange_timestamp_seconds: bucket,
                open: trade.price,
                high: trade.price,
                low: trade.price,
                close: trade.price,
                volume: trade.quantity,
            };
            RithmicLiveChartUpdate::CompleteAndStart { completed, forming }
        };
        let snapshot_generation = self
            .snapshot_generation
            .checked_add(1)
            .ok_or(RithmicLiveChartError::SnapshotGenerationOverflow)?;
        let mut bars = self.bars.clone();
        let forming = update.forming();
        let provenanced = Provenanced::new(forming, self.live_provenance(trade, forming));
        if update.completed().is_some() {
            bars.push(provenanced);
            if bars.len() > self.retained_bar_count {
                bars.remove(0);
            }
        } else if let Some(last) = bars.last_mut() {
            *last = provenanced;
        } else {
            return Err(RithmicLiveChartError::EmptyHistory);
        }
        let snapshot = ReplaySnapshot::try_from_provenanced_values(
            self.instrument.clone(),
            self.replay_provenance,
            self.bar_definition.clone(),
            snapshot_generation,
            bars.clone(),
        )
        .map_err(|_| RithmicLiveChartError::SnapshotInvalid)?;
        self.forming = forming;
        self.bars = bars;
        self.snapshot_generation = snapshot_generation;
        self.live_session_generation = Some(trade.metadata.session_generation);
        self.last_trade_sequence = Some(trade.metadata.source_sequence);
        Ok(RithmicLiveChartPublication {
            generation: self.generation,
            update,
            snapshot,
        })
    }

    fn validate_ordering(&self, trade: &MarketTrade) -> Result<(), RithmicLiveChartError> {
        if let Some(session_generation) = self.live_session_generation
            && trade.metadata.session_generation != session_generation
        {
            return Err(RithmicLiveChartError::SessionGenerationChanged);
        }
        if self
            .last_trade_sequence
            .is_some_and(|sequence| trade.metadata.source_sequence <= sequence)
        {
            return Err(RithmicLiveChartError::OutOfOrderTrade);
        }
        Ok(())
    }

    fn live_provenance(&self, trade: &MarketTrade, forming: MarketBar) -> MarketEventProvenance {
        let exchange = forming
            .exchange_timestamp_seconds
            .saturating_mul(NANOS_PER_SECOND);
        let provider = trade
            .metadata
            .timestamps
            .provider_unix_nanos
            .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
        MarketEventProvenance {
            event_id: format!(
                "rithmic_live_bar_{}_{}",
                forming.exchange_timestamp_seconds, trade.metadata.source_sequence
            ),
            event_time_unix_nanos: exchange,
            publication_time_unix_nanos: trade.metadata.timestamps.received_unix_nanos,
            producer: "axiusflow_desktop_rithmic_live_chart".to_string(),
            schema_version: self.schema_version,
            correlation_id: format!(
                "rithmic_selection_{}_series_{}",
                self.generation.selection, self.generation.series
            ),
            causation_id: trade.trade_id.clone(),
            entitlement_revision: self.entitlement_id.clone(),
            partition_id: self.partition_id,
            ownership_epoch: self.ownership_epoch,
            source_id: self.provider_id.clone(),
            source_sequence: forming.source_sequence,
            exchange_timestamp_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: provider,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: trade.metadata.timestamps.received_unix_nanos,
            normalized_timestamp_unix_nanos: trade.metadata.timestamps.received_unix_nanos,
            fanout_enqueue_timestamp_unix_nanos: None,
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_application::{MarketEventProvenance, ReplayProvenance};
    use axiusflow_instruments::{
        AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
    };
    use axiusflow_market_data::{AggressorSide, BarDefinition, EventMetadata, QualifiedTimestamp};

    const START: i64 = 1_800_000_000;

    fn generation(selection: usize, series: usize) -> RithmicChartGeneration {
        RithmicChartGeneration {
            selection: NonZeroUsize::new(selection).expect("selection is nonzero"),
            series: NonZeroUsize::new(series).expect("series is nonzero"),
        }
    }

    fn snapshot() -> ReplaySnapshot {
        let instrument = InstrumentRevision {
            instrument_id: InstrumentId::try_new("instrument:rithmic:mnqu6")
                .expect("instrument validates"),
            revision: 1,
            asset_class: AssetClass::Future,
            symbol: "MNQU6".to_string(),
            venue_id: "CME".to_string(),
            trading_currency: "USD".to_string(),
            precision: InstrumentPrecision::try_new(2, 0).expect("precision validates"),
            lifecycle: InstrumentLifecycle::Active,
        };
        let bars = [
            MarketBar {
                source_sequence: 40,
                exchange_timestamp_seconds: START - 60,
                open: 20_000,
                high: 20_020,
                low: 19_990,
                close: 20_010,
                volume: 8,
            },
            MarketBar {
                source_sequence: 41,
                exchange_timestamp_seconds: START,
                open: 20_010,
                high: 20_030,
                low: 20_000,
                close: 20_020,
                volume: 10,
            },
        ];
        let provenanced = bars
            .into_iter()
            .map(|bar| {
                let timestamp = bar.exchange_timestamp_seconds * NANOS_PER_SECOND;
                axiusflow_application::Provenanced::new(
                    bar,
                    MarketEventProvenance {
                        event_id: format!("history_{}", bar.source_sequence),
                        event_time_unix_nanos: timestamp,
                        publication_time_unix_nanos: timestamp + 1,
                        producer: "test".to_string(),
                        schema_version: 1,
                        correlation_id: "history".to_string(),
                        causation_id: String::new(),
                        entitlement_revision: "rithmic_test_cme".to_string(),
                        partition_id: 0,
                        ownership_epoch: 1,
                        source_id: "rithmic".to_string(),
                        source_sequence: bar.source_sequence,
                        exchange_timestamp_unix_nanos: timestamp,
                        provider_receive_timestamp_unix_nanos: timestamp + 1,
                        nic_receive_timestamp_unix_nanos: None,
                        axiusflow_receive_timestamp_unix_nanos: timestamp + 2,
                        normalized_timestamp_unix_nanos: timestamp + 2,
                        fanout_enqueue_timestamp_unix_nanos: None,
                        correction_flags: 0,
                        quality_flags: 0,
                        nic_timestamp_source: 0,
                        semantic_class: 2,
                    },
                )
            })
            .collect();
        ReplaySnapshot::try_from_provenanced_values(
            instrument,
            ReplayProvenance::LiveProvider,
            BarDefinition {
                definition_id: "rithmic:1m:unadjusted:v1".to_string(),
                version: 1,
                interval_seconds: 60,
            },
            1,
            provenanced,
        )
        .expect("snapshot validates")
    }

    fn trade(sequence: u64, session: u64, second: i64, price: i64, quantity: i64) -> MarketTrade {
        let timestamp = second * NANOS_PER_SECOND;
        MarketTrade {
            metadata: EventMetadata {
                provider_id: "rithmic".to_string(),
                instrument_id: "instrument:rithmic:mnqu6".to_string(),
                entitlement_id: "rithmic_test_cme".to_string(),
                source_sequence: sequence,
                session_generation: session,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: Some(timestamp),
                    provider_unix_nanos: Some(timestamp + 1),
                    received_unix_nanos: timestamp + 2,
                },
            },
            trade_id: format!("trade-{sequence}"),
            price,
            quantity,
            aggressor: AggressorSide::Buy,
        }
    }

    #[test]
    fn same_bucket_trades_coalesce_into_one_forming_replacement() {
        let fence = generation(3, 7);
        let mut chart = RithmicLiveChart::from_history(fence, &snapshot()).expect("seed validates");

        let first = chart
            .apply_trade(fence, &trade(100, 9, START + 20, 20_040, 3))
            .expect("first trade applies");
        let second = chart
            .apply_trade(fence, &trade(101, 9, START + 40, 19_980, 4))
            .expect("second trade applies");

        assert_eq!(first.update.bar_count(), 1);
        assert_eq!(second.update.bar_count(), 1);
        assert_eq!(second.update.completed(), None);
        assert_eq!(second.update.forming().source_sequence, 41);
        assert_eq!(second.update.forming().open, 20_010);
        assert_eq!(second.update.forming().high, 20_040);
        assert_eq!(second.update.forming().low, 19_980);
        assert_eq!(second.update.forming().close, 19_980);
        assert_eq!(second.update.forming().volume, 17);
        assert_eq!(first.snapshot.evidence().generation, 2);
        assert_eq!(second.snapshot.evidence().generation, 3);
        assert_eq!(second.snapshot.bars().len(), 2);
        assert_eq!(
            second
                .snapshot
                .bars()
                .last()
                .expect("forming bar is retained")
                .provenance()
                .causation_id,
            "trade-101"
        );
    }

    #[test]
    fn first_trade_in_a_new_bucket_completes_then_starts_exactly_one_bar() {
        let fence = generation(3, 7);
        let mut chart = RithmicLiveChart::from_history(fence, &snapshot()).expect("seed validates");
        let update = chart
            .apply_trade(fence, &trade(100, 9, START + 60, 20_050, 2))
            .expect("trade applies");

        assert_eq!(update.update.bar_count(), 2);
        let completed = update
            .update
            .completed()
            .expect("prior forming bar completes");
        assert_eq!(completed.source_sequence, 41);
        assert_eq!(completed.exchange_timestamp_seconds, START);
        let forming = update.update.forming();
        assert_eq!(forming.source_sequence, 42);
        assert_eq!(forming.exchange_timestamp_seconds, START + 60);
        assert_eq!(forming.open, 20_050);
        assert_eq!(forming.high, 20_050);
        assert_eq!(forming.low, 20_050);
        assert_eq!(forming.close, 20_050);
        assert_eq!(forming.volume, 2);
        assert_eq!(chart.forming(), forming);
        assert_eq!(update.generation, fence);
        assert_eq!(
            update.snapshot.instrument().instrument_id.as_str(),
            "instrument:rithmic:mnqu6"
        );
        assert_eq!(
            update.snapshot.bar_definition().definition_id,
            "rithmic:1m:unadjusted:v1"
        );
    }

    #[test]
    fn selection_and_series_replacements_are_generation_fenced() {
        let accepted = generation(3, 7);
        let mut chart =
            RithmicLiveChart::from_history(accepted, &snapshot()).expect("seed validates");
        for stale in [generation(2, 7), generation(3, 6), generation(4, 7)] {
            assert_eq!(
                chart.apply_trade(stale, &trade(100, 9, START + 20, 20_040, 3)),
                Err(RithmicLiveChartError::StaleGeneration)
            );
        }
        let seed = snapshot();
        let expected = *seed.bars().last().expect("history is nonempty").value();
        assert_eq!(chart.forming(), expected);
    }

    #[test]
    fn duplicate_out_of_order_and_old_bucket_trades_do_not_mutate_the_bar() {
        let fence = generation(3, 7);
        let mut chart = RithmicLiveChart::from_history(fence, &snapshot()).expect("seed validates");
        chart
            .apply_trade(fence, &trade(100, 9, START + 20, 20_040, 3))
            .expect("first trade applies");
        let accepted = chart.forming();

        for rejected in [
            trade(100, 9, START + 30, 20_060, 5),
            trade(99, 9, START + 40, 20_060, 5),
            trade(101, 9, START - 60, 20_060, 5),
        ] {
            assert_eq!(
                chart.apply_trade(fence, &rejected),
                Err(RithmicLiveChartError::OutOfOrderTrade)
            );
            assert_eq!(chart.forming(), accepted);
        }
    }

    #[test]
    fn session_change_and_identity_mismatch_require_recovery_without_mutation() {
        let fence = generation(3, 7);
        let mut chart = RithmicLiveChart::from_history(fence, &snapshot()).expect("seed validates");
        chart
            .apply_trade(fence, &trade(100, 9, START + 20, 20_040, 3))
            .expect("first trade applies");
        let accepted = chart.forming();

        assert_eq!(
            chart.apply_trade(fence, &trade(101, 10, START + 30, 20_050, 2)),
            Err(RithmicLiveChartError::SessionGenerationChanged)
        );
        let mut wrong_instrument = trade(101, 9, START + 30, 20_050, 2);
        wrong_instrument.metadata.instrument_id = "instrument:rithmic:esu6".to_string();
        assert_eq!(
            chart.apply_trade(fence, &wrong_instrument),
            Err(RithmicLiveChartError::IdentityMismatch)
        );
        assert_eq!(chart.forming(), accepted);
    }

    #[test]
    fn invalid_trade_failures_are_atomic_and_do_not_consume_sequence() {
        let fence = generation(3, 7);
        let mut chart = RithmicLiveChart::from_history(fence, &snapshot()).expect("seed validates");
        let accepted = chart.forming();

        let mut missing_timestamp = trade(100, 9, START + 20, 20_040, 3);
        missing_timestamp.metadata.timestamps.exchange_unix_nanos = None;
        assert_eq!(
            chart.apply_trade(fence, &missing_timestamp),
            Err(RithmicLiveChartError::MissingExchangeTimestamp)
        );
        assert_eq!(
            chart.apply_trade(fence, &trade(100, 9, START + 20, 20_040, i64::MAX)),
            Err(RithmicLiveChartError::VolumeOverflow)
        );
        assert_eq!(chart.forming(), accepted);

        let publication = chart
            .apply_trade(fence, &trade(100, 9, START + 20, 20_040, 3))
            .expect("failed updates do not consume the trade sequence");
        assert_eq!(publication.snapshot.evidence().generation, 2);
    }

    #[test]
    fn emitted_updates_are_strictly_bounded() {
        let fence = generation(3, 7);
        let mut chart = RithmicLiveChart::from_history(fence, &snapshot()).expect("seed validates");
        for index in 0..1_000_u64 {
            let update = chart
                .apply_trade(
                    fence,
                    &trade(
                        100 + index,
                        9,
                        START + 60 * i64::try_from(index + 1).expect("index fits"),
                        20_000 + i64::try_from(index).expect("index fits"),
                        1,
                    ),
                )
                .expect("ordered trade applies");
            assert!(update.update.bar_count() <= 2);
            assert_eq!(update.snapshot.bars().len(), 2);
            assert_eq!(update.snapshot.evidence().generation, 2 + index);
        }
    }
}
