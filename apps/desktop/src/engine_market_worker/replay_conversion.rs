//! Replay conversion.

use super::{
    AssetClass, BarDefinition, BarPeriod, BarSeriesKey, ChartInterval, DesktopMarketGeneration,
    InstallProviderInstrument, InstrumentId, InstrumentLifecycle, InstrumentPrecision,
    InstrumentRevision, MarketBar, MarketEventProvenance, MarketOrderBookSnapshot,
    MarketSeriesSnapshot, MarketSeriesUpdate, Provenanced, ReplayProvenance, ReplaySnapshot,
    ReplayTailOperation, ReplayTailUpdate, SeriesTailOperation, now_unix_nanos,
    provider_display_name,
};
use tradingplot_terminal_ui::{OrderBookFrame, OrderBookSelection, project_order_book};

pub(crate) fn replay_runtime_snapshot(
    publication: &MarketSeriesSnapshot,
) -> Result<ReplaySnapshot, String> {
    let snapshot = publication.snapshot.as_ref();
    let series = &snapshot.series;
    let (venue, symbol, asset_class, trading_currency) = snapshot_instrument(series)?;
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(series.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: u64::from(series.definition_version),
        asset_class,
        symbol,
        venue_id: venue,
        trading_currency,
        precision: InstrumentPrecision::try_new(snapshot.price_scale, snapshot.quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let definition = replay_bar_definition(series)?;
    let received = now_unix_nanos();
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

pub(crate) fn runtime_generation_from_snapshot(
    publication: &MarketSeriesSnapshot,
    replay: &ReplaySnapshot,
) -> Result<DesktopMarketGeneration, String> {
    let first_sequence = replay
        .bars()
        .first()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "runtime snapshot is empty".to_string())?;
    let last_sequence = replay
        .bars()
        .last()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "runtime snapshot is empty".to_string())?;
    DesktopMarketGeneration::try_new(
        publication.snapshot.provider_generation.0.get(),
        publication.publication_generation,
        first_sequence,
        last_sequence,
        replay.bars().to_vec(),
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn replay_runtime_tail_update(
    update: &MarketSeriesUpdate,
) -> Result<ReplayTailUpdate, String> {
    let item = provenanced_runtime_bar(
        &update.series,
        update.provider_generation.0.get(),
        update.consumer_id.0.get(),
        update.generation.0.get(),
        &update.bar,
        now_unix_nanos(),
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
    project_order_book(
        &OrderBookSelection {
            provider_id: publication.provider_id.clone(),
            instrument_id: publication.instrument_id.clone(),
            entitlement_id: publication.entitlement_id.clone(),
            session_generation: publication.session_generation,
            selection_generation: instrument.selection_generation,
            precision,
            price_increment: instrument.price_increment,
        },
        publication,
    )
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
            producer: "tradingplot_engine".to_string(),
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
            tradingplot_receive_timestamp_unix_nanos: received,
            normalized_timestamp_unix_nanos: received,
            fanout_enqueue_timestamp_unix_nanos: Some(received),
            correction_flags: 0,
            quality_flags: 0,
            nic_timestamp_source: 0,
            semantic_class: 2,
        },
    )
}

/// Splits a canonical engine instrument identity into presentation metadata.
///
/// Rithmic identities carry `instrument:rithmic:VENUE:SYMBOL`; Hyperliquid
/// identities carry `hyperliquid:perp:COIN`, `hyperliquid:spot:INDEX:BASE/QUOTE`,
/// or `hyperliquid:builder:DEX:COIN`. Anything else fails closed instead of
/// rendering a misrouted instrument.
pub(super) fn snapshot_instrument(
    series: &BarSeriesKey,
) -> Result<(String, String, AssetClass, String), String> {
    if series.provider_id == "rithmic" {
        let (venue, symbol) = series
            .instrument_id
            .strip_prefix("instrument:rithmic:")
            .and_then(|value| value.split_once(':'))
            .filter(|(venue, symbol)| {
                !venue.is_empty() && !symbol.is_empty() && !symbol.contains(':')
            })
            .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
        return Ok((
            venue.to_string(),
            symbol.to_string(),
            AssetClass::Future,
            "USD".to_string(),
        ));
    }
    let path = series
        .instrument_id
        .strip_prefix("hyperliquid:")
        .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
    let (kind, rest) = path
        .split_once(':')
        .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
    match kind {
        "perp" if !rest.is_empty() && !rest.contains(':') && !rest.contains('/') => Ok((
            "Hyperliquid".to_string(),
            rest.to_string(),
            AssetClass::Future,
            "USDC".to_string(),
        )),
        "spot" => {
            let (_, pair) = rest
                .split_once(':')
                .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
            let quote = pair
                .split_once('/')
                .filter(|(base, quote)| !base.is_empty() && !quote.is_empty())
                .map(|(_, quote)| quote)
                .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
            Ok((
                "Hyperliquid Spot".to_string(),
                pair.to_string(),
                AssetClass::CryptoAsset,
                quote.to_string(),
            ))
        }
        "builder" => {
            let (dex, coin) = rest
                .split_once(':')
                .filter(|(dex, coin)| !dex.is_empty() && !coin.is_empty())
                .ok_or_else(|| "engine snapshot instrument identity is invalid".to_string())?;
            Ok((
                dex.to_string(),
                format!("{dex}:{coin}"),
                AssetClass::Future,
                "USDC".to_string(),
            ))
        }
        _ => Err("engine snapshot instrument identity is invalid".to_string()),
    }
}

pub(crate) fn series_key(
    product: &InstallProviderInstrument,
    interval: ChartInterval,
) -> Result<BarSeriesKey, String> {
    // The provider field must agree with the installed instrument identity:
    // a Hyperliquid product can never demand a Rithmic series and vice
    // versa, so a mismatch fails here instead of misrouting demand.
    if product.provider != "rithmic" && product.provider != "hyperliquid"
        || product.venue_id.trim().is_empty()
        || product.price_scale > 18
        || product.quantity_scale > 18
    {
        return Err(format!(
            "{} installed instrument identity is invalid",
            provider_display_name(product.provider.as_str())
        ));
    }
    if product.provider == "rithmic" {
        if !product.entitlement_id.starts_with("rithmic-test:") {
            return Err("Rithmic installed instrument identity is invalid".to_string());
        }
    } else if product.entitlement_id != "hyperliquid-public" {
        return Err("Hyperliquid installed instrument identity is invalid".to_string());
    }
    if product.provider == "rithmic" && !product.instrument_id.starts_with("instrument:rithmic:")
        || product.provider == "hyperliquid" && !product.instrument_id.starts_with("hyperliquid:")
    {
        return Err(format!(
            "{} installed instrument identity is invalid",
            provider_display_name(product.provider.as_str())
        ));
    }
    let period = match interval {
        ChartInterval::Minute1 => BarPeriod::time(60),
        ChartInterval::Minute3 => BarPeriod::time(180),
        ChartInterval::Minute5 => BarPeriod::time(300),
        ChartInterval::Minute15 => BarPeriod::time(900),
        ChartInterval::Minute30 => BarPeriod::time(1_800),
        ChartInterval::Hour1 => BarPeriod::time(3_600),
        ChartInterval::Hour2 => BarPeriod::time(7_200),
        ChartInterval::Hour4 => BarPeriod::time(14_400),
        ChartInterval::Hour8 => BarPeriod::time(28_800),
        ChartInterval::Hour12 => BarPeriod::time(43_200),
        ChartInterval::Day1 => BarPeriod::time(86_400),
        ChartInterval::Week1 => BarPeriod::week(1),
        ChartInterval::Month1 => BarPeriod::month(1),
        // Hyperliquid serves a native 3-day candle; Rithmic has no Day3
        // series. Tick candles exist on neither public path: Hyperliquid
        // exposes no tick history and the Rithmic test feed prints none.
        ChartInterval::Day3 if product.provider == "hyperliquid" => BarPeriod::session(3),
        ChartInterval::Tick100 | ChartInterval::Day3 => {
            return Err(format!(
                "{} chart interval is unsupported",
                provider_display_name(product.provider.as_str())
            ));
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(BarSeriesKey {
        provider_id: product.provider.clone(),
        instrument_id: product.instrument_id.clone(),
        entitlement_id: product.entitlement_id.clone(),
        period,
        definition_version: 1,
    })
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
