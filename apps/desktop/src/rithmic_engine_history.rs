use crate::rithmic_history::RithmicSeriesRequest;

use axiusflow_application::{
    MarketEventProvenance, MarketGeneration, Provenanced, ProvenancedMarketBar, ReplayProvenance,
    ReplaySnapshot, ReplayTailOperation, ReplayTailUpdate,
};
use axiusflow_engine_protocol::{
    DemandError, FailureStage, InstallProviderInstrument,
    OrderBookSnapshot as IpcOrderBookSnapshot, OrderBookState as IpcOrderBookState, SeriesCadence,
    SeriesKey, SeriesSnapshot, SeriesUpdate, SeriesUpdateOperation,
};
use axiusflow_instruments::{
    AssetClass, InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision,
};
use axiusflow_market_data::{
    BarDefinition, ChartAggregation, ChartInterval, DepthLevel, MarketBar, OrderBookPublication,
    OrderBookRecoveryReason, OrderBookState,
};
use axiusflow_terminal_ui::{OrderBookFrame, OrderBookSelection, ReadOnlyOrderBook};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use axiusflow_desktop::market_worker::MarketWorkerBootstrap;

pub(crate) const MAXIMUM_VISIBLE_BARS: usize = 300;
const MAXIMUM_ORDER_BOOK_LEVELS: usize = 50;

pub(crate) fn validate_engine_instrument(
    instrument: &InstallProviderInstrument,
) -> Result<(), String> {
    if instrument.provider != "rithmic"
        || instrument.instrument_id.trim().is_empty()
        || instrument.provider_symbol.trim().is_empty()
        || instrument.display_symbol.trim().is_empty()
        || instrument.venue_id.trim().is_empty()
        || instrument.entitlement_id.trim().is_empty()
        || instrument.price_scale > 18
        || instrument.quantity_scale > 18
    {
        return Err("Rithmic engine instrument is invalid".to_string());
    }
    Ok(())
}

pub(crate) fn engine_series(
    request: RithmicSeriesRequest,
    instrument: &InstallProviderInstrument,
) -> Result<SeriesKey, String> {
    let (cadence, cadence_value) = match request.series.interval() {
        ChartInterval::Tick100 => (SeriesCadence::Trades, 100),
        ChartInterval::Day1 => (SeriesCadence::SessionDays, 1),
        ChartInterval::Day3 => (SeriesCadence::SessionDays, 3),
        ChartInterval::Week1 => (SeriesCadence::CalendarWeeks, 1),
        ChartInterval::Month1 => (SeriesCadence::CalendarMonths, 1),
        interval => match interval.aggregation() {
            ChartAggregation::FixedSeconds(seconds) => (SeriesCadence::FixedSeconds, seconds.get()),
            ChartAggregation::Trades(_) | ChartAggregation::CalendarMonth => {
                return Err("Rithmic series cadence is invalid".to_string());
            }
        },
    };
    Ok(SeriesKey {
        provider: "rithmic".to_string(),
        instrument_id: instrument.instrument_id.clone(),
        cadence_value,
        definition_revision: 1,
        entitlement_id: instrument.entitlement_id.clone(),
        cadence: cadence as i32,
    })
}

pub(crate) fn snapshot_bootstrap(
    request: RithmicSeriesRequest,
    instrument: &InstallProviderInstrument,
    snapshot: &SeriesSnapshot,
) -> Result<MarketWorkerBootstrap, String> {
    if snapshot.provider_generation < instrument.session_generation
        || snapshot.bars.is_empty()
        || snapshot.bars.len() > MAXIMUM_VISIBLE_BARS
        || snapshot.price_scale != instrument.price_scale
        || snapshot.quantity_scale != instrument.quantity_scale
    {
        return Err("Rithmic engine snapshot is invalid".to_string());
    }
    let price_scale = u8::try_from(instrument.price_scale)
        .map_err(|_| "Rithmic engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(instrument.quantity_scale)
        .map_err(|_| "Rithmic engine quantity scale is invalid".to_string())?;
    let revision = InstrumentRevision {
        instrument_id: InstrumentId::try_new(instrument.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: 1,
        asset_class: AssetClass::Future,
        symbol: instrument.display_symbol.clone(),
        venue_id: instrument.venue_id.clone(),
        trading_currency: "USD".to_string(),
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let (interval_seconds, trades_per_bar, calendar_months) =
        match request.series.interval().aggregation() {
            ChartAggregation::FixedSeconds(seconds) => (seconds.get(), None, None),
            ChartAggregation::Trades(trades) => (0, Some(trades.get()), None),
            ChartAggregation::CalendarMonth => (0, None, Some(1)),
        };
    let definition = BarDefinition {
        definition_id: format!("rithmic:{}:unadjusted:v1", request.series.label()),
        version: 1,
        interval_seconds,
        trades_per_bar,
        calendar_months,
    };
    let received = unix_nanos_now()?;
    let bars = provenanced_engine_bars(request, instrument, snapshot, received);
    let first_sequence = bars
        .first()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "Rithmic engine snapshot is empty".to_string())?;
    let last_sequence = bars
        .last()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "Rithmic engine snapshot is empty".to_string())?;
    let generation = MarketGeneration::try_new(
        snapshot.provider_generation,
        snapshot.publication_generation,
        first_sequence,
        last_sequence,
        bars.clone(),
    )
    .map_err(|error| error.to_string())?;
    let replay = ReplaySnapshot::try_from_provenanced_values(
        revision,
        ReplayProvenance::LiveProvider,
        definition,
        snapshot.publication_generation,
        bars,
    )
    .map_err(|error| error.to_string())?;
    Ok(MarketWorkerBootstrap {
        snapshot: replay,
        subscription_id: format!(
            "{}  ·  {}",
            instrument.provider_symbol,
            request.series.label()
        ),
        generation,
        worker_label: format!("Rithmic Test · {} engine history", request.series.label()),
    })
}

fn provenanced_engine_bars(
    request: RithmicSeriesRequest,
    instrument: &InstallProviderInstrument,
    snapshot: &SeriesSnapshot,
    received: i64,
) -> Vec<ProvenancedMarketBar> {
    snapshot
        .bars
        .iter()
        .map(|bar| {
            let bar = MarketBar {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                exchange_timestamp_unix_nanos: bar.exchange_timestamp_unix_nanos,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
            };
            let exchange = bar.exchange_timestamp_unix_nanos;
            Provenanced::new(
                bar,
                MarketEventProvenance {
                    event_id: format!(
                        "engine-rithmic-{}-{}-{}",
                        snapshot.provider_generation, snapshot.generation, bar.source_sequence
                    ),
                    event_time_unix_nanos: exchange,
                    publication_time_unix_nanos: received,
                    producer: "axiusflow_engine".to_string(),
                    schema_version: 1,
                    correlation_id: format!(
                        "rithmic-selection-{}-series-{}",
                        request.selection_generation, request.series_generation
                    ),
                    causation_id: "resident_engine_history".to_string(),
                    entitlement_revision: instrument.entitlement_id.clone(),
                    session_generation: snapshot.provider_generation,
                    source_id: "rithmic".to_string(),
                    source_sequence: bar.source_sequence,
                    exchange_timestamp_unix_nanos: exchange,
                    provider_receive_timestamp_unix_nanos: received,
                    nic_receive_timestamp_unix_nanos: None,
                    axiusflow_receive_timestamp_unix_nanos: received,
                    normalized_timestamp_unix_nanos: received,
                    fanout_enqueue_timestamp_unix_nanos: Some(received),
                    correction_flags: 0,
                    quality_flags: 0,
                    nic_timestamp_source: 0,
                    semantic_class: 2,
                },
            )
        })
        .collect()
}

pub(crate) fn live_tail(
    request: RithmicSeriesRequest,
    instrument: &InstallProviderInstrument,
    update: &SeriesUpdate,
) -> Result<ReplayTailUpdate, String> {
    if update.provider_generation < instrument.session_generation {
        return Err("Rithmic engine update provider generation is stale".to_string());
    }
    let source = update
        .bar
        .as_ref()
        .ok_or_else(|| "Rithmic engine update has no bar".to_string())?;
    let received = unix_nanos_now()?;
    let bar = MarketBar {
        source_sequence: source.source_sequence,
        exchange_timestamp_seconds: source.exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos: source.exchange_timestamp_unix_nanos,
        open: source.open,
        high: source.high,
        low: source.low,
        close: source.close,
        volume: source.volume,
    };
    let exchange = bar.exchange_timestamp_unix_nanos;
    let item = Provenanced::new(
        bar,
        MarketEventProvenance {
            event_id: format!(
                "engine-rithmic-{}-{}-{}",
                update.provider_generation, update.generation, bar.source_sequence
            ),
            event_time_unix_nanos: exchange,
            publication_time_unix_nanos: received,
            producer: "axiusflow_engine".to_string(),
            schema_version: 1,
            correlation_id: format!(
                "rithmic-selection-{}-series-{}",
                request.selection_generation, request.series_generation
            ),
            causation_id: "resident_engine_live_tail".to_string(),
            entitlement_revision: instrument.entitlement_id.clone(),
            session_generation: update.provider_generation,
            source_id: "rithmic".to_string(),
            source_sequence: bar.source_sequence,
            exchange_timestamp_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: received,
            nic_receive_timestamp_unix_nanos: None,
            axiusflow_receive_timestamp_unix_nanos: received,
            normalized_timestamp_unix_nanos: received,
            fanout_enqueue_timestamp_unix_nanos: Some(received),
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    );
    let operation = match SeriesUpdateOperation::try_from(update.operation) {
        Ok(SeriesUpdateOperation::ReviseTail) => ReplayTailOperation::Revise,
        Ok(SeriesUpdateOperation::AppendTail) => ReplayTailOperation::Append,
        Ok(SeriesUpdateOperation::Unspecified) | Err(_) => {
            return Err("Engine series update operation is invalid".to_string());
        }
    };
    ReplayTailUpdate::try_new(
        item,
        update.publication_generation,
        update.forming,
        operation,
    )
    .map_err(|error| error.to_string())
}

pub(crate) struct OrderBookIdentity<'a> {
    pub instrument: &'a InstallProviderInstrument,
    pub series_generation: u64,
}

pub(crate) fn order_book_from_snapshot(
    identity: &OrderBookIdentity<'_>,
    snapshot: &IpcOrderBookSnapshot,
) -> Result<OrderBookFrame, String> {
    let instrument = identity.instrument;
    if snapshot.consumer_id == 0
        || snapshot.generation != identity.series_generation
        || snapshot.provider != instrument.provider
        || snapshot.instrument_id != instrument.instrument_id
        || snapshot.entitlement_id != instrument.entitlement_id
        || snapshot.provider_generation < instrument.session_generation
        || snapshot.bids.len() > MAXIMUM_ORDER_BOOK_LEVELS
        || snapshot.asks.len() > MAXIMUM_ORDER_BOOK_LEVELS
    {
        return Err("Engine order-book identity is invalid".to_string());
    }
    let state = match IpcOrderBookState::try_from(snapshot.state)
        .map_err(|_| "Engine order-book state is invalid".to_string())?
    {
        IpcOrderBookState::Unspecified => {
            return Err("Engine order-book state is unspecified".to_string());
        }
        IpcOrderBookState::AwaitingSnapshot => {
            OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot)
        }
        IpcOrderBookState::Ready => OrderBookState::Ready,
        IpcOrderBookState::Stale => OrderBookState::Stale,
        IpcOrderBookState::SequenceGap => {
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        }
        IpcOrderBookState::CrossedBook => {
            OrderBookState::Recovering(OrderBookRecoveryReason::CrossedBook)
        }
        IpcOrderBookState::InvalidUpdate => {
            OrderBookState::Recovering(OrderBookRecoveryReason::InvalidUpdate)
        }
    };
    let bids = ipc_depth_levels(&snapshot.bids, true)?;
    let asks = ipc_depth_levels(&snapshot.asks, false)?;
    let best_bid = snapshot
        .best_bid
        .as_ref()
        .map(ipc_depth_level)
        .transpose()?;
    let best_ask = snapshot
        .best_ask
        .as_ref()
        .map(ipc_depth_level)
        .transpose()?;
    if best_bid
        .zip(best_ask)
        .is_some_and(|(bid, ask)| bid.price >= ask.price)
    {
        return Err("Engine BBO is crossed".to_string());
    }
    let traded_volumes = snapshot
        .bids
        .iter()
        .chain(&snapshot.asks)
        .filter(|level| level.traded_volume > 0)
        .map(|level| (level.price, level.traded_volume))
        .collect::<BTreeMap<_, _>>();
    if bids
        .first()
        .zip(asks.first())
        .is_some_and(|(bid, ask)| bid.price >= ask.price)
    {
        return Err("Engine order book is crossed".to_string());
    }
    let publication = OrderBookPublication {
        provider_id: snapshot.provider.clone(),
        instrument_id: snapshot.instrument_id.clone(),
        entitlement_id: snapshot.entitlement_id.clone(),
        session_generation: snapshot.provider_generation,
        revision: snapshot.revision,
        source_watermark: snapshot.source_watermark,
        best_bid,
        best_ask,
        bbo_source_watermark: snapshot.bbo_source_watermark,
        bids,
        asks,
        traded_volumes,
        state,
    };
    let selection = OrderBookSelection {
        provider_id: snapshot.provider.clone(),
        instrument_id: snapshot.instrument_id.clone(),
        entitlement_id: snapshot.entitlement_id.clone(),
        session_generation: snapshot.provider_generation,
        // The snapshot's consumer and demand generation already fence the
        // active chart. Catalog selection generations are consumer-local, but
        // the resident engine's canonical book is shared by instrument, so the
        // projection must retain this consumer's selection identity.
        selection_generation: instrument.selection_generation,
        precision: InstrumentPrecision::try_new(
            u8::try_from(instrument.price_scale)
                .map_err(|_| "Engine price scale is invalid".to_string())?,
            u8::try_from(instrument.quantity_scale)
                .map_err(|_| "Engine quantity scale is invalid".to_string())?,
        )
        .map_err(|error| error.to_string())?,
    };
    ReadOnlyOrderBook::project_publication(&selection, &publication)
        .ok_or_else(|| "Engine order-book publication is stale".to_string())
}

fn ipc_depth_levels(
    levels: &[axiusflow_engine_protocol::OrderBookLevel],
    bids: bool,
) -> Result<Vec<DepthLevel>, String> {
    let mut previous = None;
    let mut converted = Vec::with_capacity(levels.len());
    for level in levels {
        let converted_level = ipc_depth_level(level)?;
        if previous.is_some_and(|previous| {
            if bids {
                level.price >= previous
            } else {
                level.price <= previous
            }
        }) {
            return Err("Engine order-book levels are unordered".to_string());
        }
        previous = Some(level.price);
        converted.push(converted_level);
    }
    Ok(converted)
}

fn ipc_depth_level(
    level: &axiusflow_engine_protocol::OrderBookLevel,
) -> Result<DepthLevel, String> {
    if level.price <= 0 || level.quantity <= 0 || level.traded_volume < 0 {
        return Err("Engine order-book level is invalid".to_string());
    }
    Ok(DepthLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
    })
}

/// Engine-side marker for a provider history demand that completed with no
/// bars to form them from (produced in
/// `apps/engine/src/market_service/history.rs`). The desktop matches it to
/// render the one terminal state it can diagnose precisely: the session,
/// selection, and subscription succeeded, but the feed published no prints
/// or history for the instrument.
const EMPTY_HISTORY_MARKER: &str = "historical bars are unavailable";

/// Maps a failed Rithmic history bootstrap to the terminal
/// `(series_message, chart_state_message)` pair. Every failure keeps the
/// previous chart and restates its demand at the call site; only the message
/// content varies. The empty-feed case names the cause, the corrective
/// action, and the standing chart so the trader is not left guessing. All
/// other failures keep their exact existing wording.
pub(crate) fn history_failure_messages(error: &str) -> (String, String) {
    let series_message = "Rithmic visible history is unavailable".to_string();
    if error.contains(EMPTY_HISTORY_MARKER) {
        return (
            series_message,
            format!(
                "Rithmic visible history could not be loaded: {error}. \
                 The feed published no prints or history for this instrument, \
                 so no bars can form. Check the account market-data entitlement, \
                 then select the instrument again to retry; the previous chart stays live."
            ),
        );
    }
    (
        series_message,
        format!("Rithmic visible history could not be loaded: {error}"),
    )
}

pub(crate) fn demand_error_message(error: &DemandError) -> String {
    let stage = match FailureStage::try_from(error.stage_code) {
        Ok(stage) => failure_stage_label(stage),
        Err(_) => error.stage.as_str(),
    };
    let elapsed = error
        .elapsed_millis
        .map_or(String::new(), |elapsed| format!(" after {elapsed} ms"));
    format!("{stage} failed{elapsed}: {}", error.detail)
}

const fn failure_stage_label(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::Unspecified => "market demand",
        FailureStage::ProviderHistory => "provider history",
        FailureStage::CanonicalValidation => "canonical validation",
        FailureStage::MemoryInstall => "memory install",
        FailureStage::Aggregation => "aggregation",
        FailureStage::SegmentEncode => "segment encode",
        FailureStage::Encryption => "encryption",
        FailureStage::FilesystemWrite => "filesystem write",
        FailureStage::CatalogCommit => "catalog commit",
        FailureStage::Handoff => "history/live handoff",
        FailureStage::Publication => "publication",
        FailureStage::IpcSend => "local IPC send",
        FailureStage::ChartInstall => "chart install",
        FailureStage::ProviderRealtime => "provider realtime",
    }
}

fn unix_nanos_now() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| "system clock is invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rithmic_history::RithmicSeries;
    use axiusflow_engine_protocol::MarketBar as IpcMarketBar;
    use std::num::NonZeroUsize;

    fn request(interval: ChartInterval) -> RithmicSeriesRequest {
        RithmicSeriesRequest {
            selection_generation: NonZeroUsize::new(2).expect("selection generation"),
            series_generation: NonZeroUsize::new(3).expect("series generation"),
            series: RithmicSeries::from(interval),
        }
    }

    fn installed() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: 7,
            selection_generation: 2,
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQU6".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
        }
    }

    #[test]
    fn engine_series_keys_cover_every_rithmic_interval() {
        let instrument = installed();
        for interval in ChartInterval::ALL {
            let key = engine_series(request(interval), &instrument).expect("series key validates");
            assert_eq!(key.provider, "rithmic");
            assert_ne!(key.cadence, SeriesCadence::Unspecified as i32);
        }
    }

    #[test]
    fn engine_snapshot_preserves_exact_time_and_generation() {
        let request = request(ChartInterval::Tick100);
        let instrument = installed();
        let snapshot = SeriesSnapshot {
            consumer_id: 5,
            generation: 3,
            series: Some(engine_series(request, &instrument).expect("series key")),
            provider_generation: 8,
            price_scale: 2,
            quantity_scale: 0,
            bars: vec![IpcMarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 1_700_000_000,
                exchange_timestamp_unix_nanos: 1_700_000_000_123_456_789,
                open: 10_000,
                high: 10_100,
                low: 9_900,
                close: 10_050,
                volume: 8,
            }],
            publication_generation: 4,
            forming: false,
        };
        let bootstrap = snapshot_bootstrap(request, &instrument, &snapshot).expect("bootstrap");
        assert_eq!(bootstrap.generation.session_generation(), 8);
        assert_eq!(bootstrap.generation.publication_generation(), 4);
        assert_eq!(
            bootstrap.snapshot.bars()[0]
                .provenance()
                .exchange_timestamp_unix_nanos,
            1_700_000_000_123_456_789
        );
    }

    #[test]
    fn empty_feed_failure_names_cause_action_and_standing_chart() {
        let error = "provider history failed after 12 ms: Rithmic historical bars are unavailable";
        let (series_message, chart_message) = history_failure_messages(error);
        assert_eq!(series_message, "Rithmic visible history is unavailable");
        assert!(chart_message.contains(error));
        assert!(chart_message.contains("no prints or history"));
        assert!(chart_message.contains("market-data entitlement"));
        assert!(chart_message.contains("select the instrument again to retry"));
        assert!(chart_message.contains("previous chart stays live"));
    }

    #[test]
    fn unrelated_failure_keeps_exact_existing_wording() {
        let error = "Rithmic engine snapshot is invalid";
        let (series_message, chart_message) = history_failure_messages(error);
        assert_eq!(series_message, "Rithmic visible history is unavailable");
        assert_eq!(
            chart_message,
            "Rithmic visible history could not be loaded: Rithmic engine snapshot is invalid"
        );
    }
}
