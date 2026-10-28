//! Disconnected replay source for desktop tests and explicit diagnostics.

use aeris_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    ReplayStreamUpdate,
};
use aeris_market_runtime::MarketConsumerEvent;
use std::num::NonZeroUsize;

use super::{FixtureMarketEvents, FixtureSeries};
use crate::market_worker::{
    MarketPublicationGeneration, MarketWorkerBootstrap, MarketWorkerPublication,
};

const SUBSCRIPTION_ID: &str = "desktop_fixture_market_bars";
const MODEL_ITEM_CAPACITY: usize = 600;

pub struct FixtureMarketWorker {
    source: EmbeddedReplaySource,
    model: MarketBarClientModel,
}

impl FixtureMarketWorker {
    /// Builds a canonical covering publication for the selected fixture series.
    ///
    /// # Errors
    /// Returns an error if the disconnected replay source cannot supply the bars.
    pub fn publish_runtime_snapshot(
        &mut self,
        bar_count: usize,
        route: &FixtureSeries,
        selection: u64,
        events: &FixtureMarketEvents,
    ) -> Result<MarketConsumerEvent, String> {
        let bootstrap = self.publish_snapshot(bar_count)?;
        let bars: Vec<_> = bootstrap
            .snapshot
            .bars()
            .iter()
            .map(|item| *item.value())
            .collect();
        Ok(events.snapshot(
            route,
            selection,
            bootstrap.generation.publication_generation(),
            bars,
        ))
    }

    /// Creates a deterministic disconnected fixture worker.
    ///
    /// # Errors
    /// Retained for the common worker-start contract; construction is infallible.
    pub fn try_new() -> Result<Self, String> {
        Ok(Self {
            source: EmbeddedReplaySource,
            model: MarketBarClientModel::new(
                NonZeroUsize::new(MODEL_ITEM_CAPACITY).unwrap_or(NonZeroUsize::MIN),
            ),
        })
    }

    /// Loads and publishes a bounded fixture snapshot.
    ///
    /// # Errors
    /// Returns an error if loading, decoding, validation, or publication fails.
    pub fn publish_snapshot(&mut self, bar_count: usize) -> Result<MarketWorkerBootstrap, String> {
        let snapshot = self
            .source
            .load_snapshot(LoadEmbeddedReplay { bar_count })
            .map_err(|error| error.to_string())?;
        let generation = self.model.current_generation().map_or(
            Ok(snapshot.evidence().publication_generation),
            |current| {
                current
                    .publication_generation()
                    .checked_add(1)
                    .ok_or_else(|| "desktop fixture generation overflow".to_string())
            },
        )?;
        let snapshot = snapshot
            .try_with_publication_generation(generation)
            .map_err(|error| error.to_string())?;
        let publication =
            self.finish_publication(ReplayStreamUpdate::Snapshot(snapshot.clone()))?;
        let ReplayStreamUpdate::Snapshot(decoded_snapshot) = publication.update else {
            return Err("fixture snapshot published a delta".to_string());
        };
        Ok(MarketWorkerBootstrap {
            snapshot: decoded_snapshot,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            generation: publication.generation,
            worker_label: "deterministic fixture · disconnected".to_string(),
        })
    }

    /// Loads and publishes the fixture delta following `previous_sequence`.
    ///
    /// # Errors
    /// Returns an error if loading, decoding, validation, or publication fails.
    pub fn publish_delta(
        &mut self,
        previous_sequence: u64,
    ) -> Result<Option<MarketWorkerPublication>, String> {
        let Some(delta) = self
            .source
            .load_delta(previous_sequence)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        self.finish_publication(ReplayStreamUpdate::Delta(delta))
            .map(Some)
    }

    fn finish_publication(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<MarketWorkerPublication, String> {
        let outcome = self
            .model
            .apply_update(update.clone())
            .map_err(|error| error.to_string())?;
        let MarketBarModelOutcome::Published(generation) = outcome else {
            return Err("fixture update did not publish a client generation".to_string());
        };
        let (first, last) = generation.sequence_range();
        Ok(MarketWorkerPublication {
            update,
            generation: MarketPublicationGeneration::from_tail(
                generation.publication_generation(),
                generation.items().len(),
                first,
                last,
            ),
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: "deterministic fixture · disconnected".to_string(),
            ui_diagnostics: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FixtureMarketWorker;
    use crate::test_support::{FixtureMarketEvents, FixtureSeries};
    use aeris_market_data::{BarPeriod, BarSeriesKey};
    use aeris_market_runtime::MarketRuntimeEvent;

    #[test]
    fn fixture_market_publishes_disconnected_bounded_snapshots() {
        let mut market = FixtureMarketWorker::try_new().expect("fixture constructs");
        let first = market.publish_snapshot(2).expect("first snapshot");
        assert_eq!(first.generation.retained_items(), 2);
        assert_eq!(first.subscription_id, "desktop_fixture_market_bars");
        assert!(first.worker_label.contains("disconnected"));
        let next = market.publish_snapshot(3).expect("next snapshot");
        assert_eq!(next.generation.retained_items(), 3);
        assert!(
            next.generation.publication_generation() > first.generation.publication_generation()
        );
        let series = BarSeriesKey {
            provider_id: "hyperliquid".into(),
            instrument_id: "hyperliquid:perp:BTC".into(),
            entitlement_id: "fixture".into(),
            period: BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        };
        let route = FixtureSeries {
            series: series.clone(),
            provider_generation: 1,
            price_scale: 2,
            quantity_scale: 3,
        };
        let event = market
            .publish_runtime_snapshot(3, &route, 7, &FixtureMarketEvents::default())
            .expect("canonical snapshot");
        let MarketRuntimeEvent::SeriesSnapshot(publication) = event.value else {
            panic!("expected canonical series snapshot");
        };
        assert_eq!(publication.generation.0.get(), 7);
        assert_eq!(publication.snapshot.series, series);
        assert_eq!(publication.snapshot.bars.len(), 3);
        assert_eq!(publication.snapshot.price_scale, 2);
        assert_eq!(publication.snapshot.quantity_scale, 3);
        assert!(publication.publication_generation > next.generation.publication_generation());
    }
}
