//! Replay conversion.

use super::{
    AssetClass, BarDefinition, BarPeriod, BarSeriesKey, ChartInterval, InstallProviderInstrument,
    InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision, MarketBar,
    MarketEventProvenance, MarketOrderBookSnapshot, MarketSeriesSnapshot, MarketSeriesUpdate,
    Provenanced, ReplayProvenance, ReplaySnapshot, ReplayTailOperation, ReplayTailUpdate,
    SeriesTailOperation, provider_display_name,
};
use aeris_instruments::InstrumentClass;
use aeris_market_data::{OrderQueueFrame, OrderQueueLevel, OrderQueueState};
use aeris_market_runtime::InstrumentDescriptor;
use aeris_market_runtime::{MarketOrderLevelStatus, MarketOrderLevelView, MarketOrderQueue};
use aeris_terminal_ui::{OrderBookFrame, OrderBookSelection, project_order_book};
use std::collections::BTreeMap;

pub(crate) fn replay_runtime_snapshot(
    publication: &MarketSeriesSnapshot,
    descriptor: &InstrumentDescriptor,
    received: i64,
) -> Result<ReplaySnapshot, String> {
    let snapshot = publication.snapshot.as_ref();
    let series = &snapshot.series;
    let installed = &descriptor.instrument;
    if series.provider_id != installed.provider
        || series.instrument_id != installed.instrument_id
        || series.entitlement_id != installed.entitlement_id
        || descriptor.entitlement_id != installed.entitlement_id
    {
        return Err("engine snapshot does not match the selected instrument".to_string());
    }
    let quantity_scale = u8::try_from(installed.quantity_scale)
        .map_err(|_| "selected instrument quantity precision is invalid".to_string())?;
    if snapshot.price_scale != descriptor.price_scale || snapshot.quantity_scale != quantity_scale {
        return Err("engine snapshot precision does not match the selected descriptor".into());
    }
    let asset_class = match descriptor.asset_class {
        InstrumentClass::Equity => AssetClass::Equity,
        InstrumentClass::Option => AssetClass::Option,
        InstrumentClass::Future | InstrumentClass::CryptoPerpetual => AssetClass::Future,
        InstrumentClass::ForeignExchange => AssetClass::ForeignExchange,
        InstrumentClass::CryptoSpot => AssetClass::CryptoAsset,
        InstrumentClass::Other => return Err("engine snapshot instrument class is unknown".into()),
    };
    let trading_currency = descriptor
        .currency
        .clone()
        .filter(|currency| !currency.trim().is_empty())
        .or_else(|| {
            installed
                .contract_metadata
                .as_deref()
                .and_then(|terms| terms.currency.clone())
        })
        .filter(|currency| !currency.trim().is_empty())
        .ok_or_else(|| "engine snapshot trading currency is unknown".to_string())?;
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(series.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: u64::from(series.definition_version),
        asset_class,
        symbol: descriptor.display_symbol.clone(),
        venue_id: descriptor.venue.clone(),
        trading_currency,
        precision: InstrumentPrecision::try_new(descriptor.price_scale, quantity_scale)
            .map_err(|error| error.to_string())?,
        price_increment: installed.price_increment,
        lifecycle: InstrumentLifecycle::Active,
        contract: None,
    };
    let definition = replay_bar_definition(series)?;
    let bars = snapshot
        .bars
        .iter()
        .map(|bar| {
            provenanced_runtime_bar(
                &snapshot.series,
                snapshot.provider_generation.0.get(),
                publication.consumer_id.0.get(),
                publication.generation.0.get(),
                bar,
                received,
            )
        })
        .collect();
    ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        publication.publication_generation,
        bars,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn replay_runtime_tail_update(
    update: &MarketSeriesUpdate,
    received: i64,
) -> Result<ReplayTailUpdate, String> {
    let item = provenanced_runtime_bar(
        &update.series,
        update.provider_generation.0.get(),
        update.consumer_id.0.get(),
        update.generation.0.get(),
        &update.bar,
        received,
    );
    let operation = match update.operation {
        SeriesTailOperation::Revise => ReplayTailOperation::Revise,
        SeriesTailOperation::Append => ReplayTailOperation::Append,
    };
    ReplayTailUpdate::try_new(
        item,
        update.publication_generation,
        update.forming,
        operation,
    )
    .map_err(|error| error.to_string())
}

/// Projects a book for the pane. The frame carries the consumer's demand generation, the
/// identity panes fence every publication with; a provider catalog numbers its own
/// instrument selections independently of that demand.
pub(crate) fn runtime_order_book_frame(
    snapshot: &MarketOrderBookSnapshot,
    instrument: &InstallProviderInstrument,
    generation: u64,
) -> Option<OrderBookFrame> {
    let canonical = &snapshot.publication;
    let mut display_publication = None;
    if let Some(display) = snapshot
        .display_depth
        .as_ref()
        .filter(|display| display.provider_generation == canonical.session_generation)
    {
        let mut publication = canonical.clone();
        publication.bids.clone_from(&display.bids);
        publication.asks.clone_from(&display.asks);
        display_publication = Some(publication);
    }
    let publication = display_publication.as_ref().unwrap_or(canonical);
    if snapshot.generation.0.get() != generation
        || publication.provider_id != instrument.provider
        || publication.instrument_id != instrument.instrument_id
        || publication.entitlement_id != instrument.entitlement_id
        || publication.session_generation < instrument.session_generation
    {
        return None;
    }
    let precision = InstrumentPrecision::try_new(
        u8::try_from(instrument.price_scale).ok()?,
        u8::try_from(instrument.quantity_scale).ok()?,
    )
    .ok()?;
    let mut frame = project_order_book(
        &OrderBookSelection {
            provider_id: publication.provider_id.clone(),
            instrument_id: publication.instrument_id.clone(),
            entitlement_id: publication.entitlement_id.clone(),
            session_generation: publication.session_generation,
            selection_generation: generation,
            precision,
            price_increment: instrument.price_increment,
        },
        publication,
    )?;
    frame.order_queues = snapshot
        .order_levels
        .as_ref()
        .map(|view| order_queue_frame(view, publication.session_generation));
    Some(frame)
}

/// Queues from another provider session than the book's are never shown as current.
fn order_queue_frame(view: &MarketOrderLevelView, session_generation: u64) -> OrderQueueFrame {
    let state = match view.status {
        MarketOrderLevelStatus::Ready if view.provider_generation == session_generation => {
            OrderQueueState::Ready
        }
        MarketOrderLevelStatus::Ready | MarketOrderLevelStatus::AwaitingSnapshot => {
            OrderQueueState::Loading
        }
        MarketOrderLevelStatus::Recovering(_) | MarketOrderLevelStatus::Stale => {
            OrderQueueState::Recovering
        }
        MarketOrderLevelStatus::Unavailable(_) => OrderQueueState::Unavailable,
        MarketOrderLevelStatus::BudgetExhausted => OrderQueueState::LimitReached,
    };
    let queues = |side: &[MarketOrderQueue]| {
        if state != OrderQueueState::Ready {
            return BTreeMap::new();
        }
        side.iter()
            .map(|queue| {
                (
                    queue.price,
                    OrderQueueLevel {
                        order_count: queue.order_count,
                        quantities: queue.orders.iter().map(|order| order.quantity).collect(),
                    },
                )
            })
            .collect()
    };
    OrderQueueFrame {
        state,
        bids: queues(&view.bids),
        asks: queues(&view.asks),
    }
}

fn provenanced_runtime_bar(
    series: &BarSeriesKey,
    provider_generation: u64,
    consumer_id: u64,
    generation: u64,
    bar: &MarketBar,
    received: i64,
) -> Provenanced<MarketBar> {
    let exchange = bar.exchange_timestamp_unix_nanos;
    Provenanced::new(
        *bar,
        MarketEventProvenance {
            event_id: format!(
                "engine-{provider_generation}-{generation}-{}",
                bar.source_sequence
            ),
            event_time_unix_nanos: exchange,
            publication_time_unix_nanos: received,
            producer: "aeris_engine".to_string(),
            schema_version: 1,
            correlation_id: format!("engine-series-{consumer_id}-{generation}"),
            causation_id: String::new(),
            entitlement_revision: series.entitlement_id.clone(),
            session_generation: provider_generation,
            source_id: series.provider_id.clone(),
            source_sequence: bar.source_sequence,
            exchange_timestamp_unix_nanos: exchange,
            provider_receive_timestamp_unix_nanos: received,
            nic_receive_timestamp_unix_nanos: None,
            aeris_receive_timestamp_unix_nanos: received,
            normalized_timestamp_unix_nanos: received,
            fanout_enqueue_timestamp_unix_nanos: Some(received),
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    )
}

pub(crate) fn series_key(
    product: &InstallProviderInstrument,
    interval: ChartInterval,
) -> Result<BarSeriesKey, String> {
    let presentation =
        super::provider_presentation(product.provider.as_str()).ok_or_else(|| {
            format!(
                "{} chart interval is unsupported",
                provider_display_name(product.provider.as_str())
            )
        })?;
    if product.entitlement_id.trim().is_empty() || product.venue_id.trim().is_empty() {
        return Err(format!(
            "{} installed instrument identity is invalid",
            provider_display_name(product.provider.as_str())
        ));
    }
    aeris_market_runtime::InstrumentDescriptor::from_install(product, presentation, true).map_err(
        |_| {
            format!(
                "{} installed instrument identity is invalid",
                provider_display_name(product.provider.as_str())
            )
        },
    )?;
    aeris_market_runtime::resolve_series_key(product, interval, presentation)
        .map_err(|error| error.to_string())
}

pub(super) fn replay_bar_definition(series: &BarSeriesKey) -> Result<BarDefinition, String> {
    let (cadence_id, interval_seconds, trades_per_bar, calendar_months) = match series.period {
        BarPeriod::Tick { trades } => (format!("trades:{trades}"), 0, Some(trades), None),
        BarPeriod::Time { seconds } => (format!("{seconds}s"), seconds, None, None),
        BarPeriod::Session { days } => {
            let seconds = days
                .checked_mul(24 * 60 * 60)
                .ok_or_else(|| "engine session cadence overflowed".to_string())?;
            (format!("session-days:{days}"), seconds, None, None)
        }
        BarPeriod::Week { weeks } => {
            let seconds = weeks
                .checked_mul(7 * 24 * 60 * 60)
                .ok_or_else(|| "engine calendar-week cadence overflowed".to_string())?;
            (format!("calendar-weeks:{weeks}"), seconds, None, None)
        }
        BarPeriod::Month { months } => (format!("calendar-months:{months}"), 0, None, Some(months)),
    };
    Ok(BarDefinition {
        definition_id: format!(
            "{}:{}:{cadence_id}",
            series.provider_id, series.instrument_id
        ),
        version: series.definition_version,
        interval_seconds,
        trades_per_bar,
        calendar_months,
    })
}
