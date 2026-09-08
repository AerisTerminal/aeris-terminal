//! Replay conversion.

use super::{
    AssetClass, BarDefinition, ChartInterval, DesktopMarketGeneration, InstallProviderInstrument,
    InstrumentId, InstrumentLifecycle, InstrumentPrecision, InstrumentRevision, MarketBar,
    MarketEventProvenance, MarketPublicationGeneration, Provenanced, RETAINED_BAR_CAPACITY,
    ReplayProvenance, ReplaySnapshot, ReplayTailOperation, ReplayTailUpdate, SeriesCadence,
    SeriesKey, SeriesSnapshot, SeriesUpdate, SeriesUpdateOperation, now_unix_nanos,
    provider_display_name,
};

pub(super) fn replay_snapshot(snapshot: &SeriesSnapshot) -> Result<ReplaySnapshot, String> {
    let series = snapshot
        .series
        .clone()
        .ok_or_else(|| "engine snapshot has no series identity".to_string())?;
    if series.provider != "rithmic" && series.provider != "hyperliquid"
        || !matches!(
            SeriesCadence::try_from(series.cadence),
            Ok(SeriesCadence::FixedSeconds
                | SeriesCadence::CalendarWeeks
                | SeriesCadence::CalendarMonths)
        )
    {
        return Err(format!(
            "engine {} snapshot identity is invalid",
            provider_display_name(series.provider.as_str())
        ));
    }
    if series.provider == "rithmic" {
        if !series.entitlement_id.starts_with("rithmic-test:")
            || !series.instrument_id.starts_with("instrument:rithmic:")
        {
            return Err("engine Rithmic snapshot identity is invalid".to_string());
        }
    } else if !series.entitlement_id.starts_with("hyperliquid-")
        || !series.instrument_id.starts_with("hyperliquid:")
    {
        return Err("engine Hyperliquid snapshot identity is invalid".to_string());
    }
    let price_scale = u8::try_from(snapshot.price_scale)
        .map_err(|_| "engine price scale is invalid".to_string())?;
    let quantity_scale = u8::try_from(snapshot.quantity_scale)
        .map_err(|_| "engine quantity scale is invalid".to_string())?;
    let (venue, symbol, asset_class, trading_currency) = snapshot_instrument(&series)?;
    let instrument = InstrumentRevision {
        instrument_id: InstrumentId::try_new(series.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        revision: u64::from(series.definition_revision),
        asset_class,
        symbol,
        venue_id: venue,
        trading_currency,
        precision: InstrumentPrecision::try_new(price_scale, quantity_scale)
            .map_err(|error| error.to_string())?,
        lifecycle: InstrumentLifecycle::Active,
    };
    let definition = replay_bar_definition(&series)?;
    let received = now_unix_nanos();
    let bars = snapshot
        .bars
        .iter()
        .map(|bar| {
            provenanced_engine_bar(
                &series,
                snapshot.provider_generation,
                snapshot.consumer_id,
                snapshot.generation,
                bar,
                received,
            )
        })
        .collect();
    ReplaySnapshot::try_from_provenanced_values(
        instrument,
        ReplayProvenance::LiveProvider,
        definition,
        snapshot.publication_generation,
        bars,
    )
    .map_err(|error| error.to_string())
}

/// Splits a canonical engine instrument identity into presentation metadata.
///
/// Rithmic identities carry `instrument:rithmic:VENUE:SYMBOL`; Hyperliquid
/// identities carry `hyperliquid:perp:COIN`, `hyperliquid:spot:INDEX:BASE/QUOTE`,
/// or `hyperliquid:builder:DEX:COIN`. Anything else fails closed instead of
/// rendering a misrouted instrument.
pub(super) fn snapshot_instrument(
    series: &SeriesKey,
) -> Result<(String, String, AssetClass, String), String> {
    if series.provider == "rithmic" {
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

pub(super) fn generation_from_snapshot(
    snapshot: &SeriesSnapshot,
    replay: &ReplaySnapshot,
) -> Result<DesktopMarketGeneration, String> {
    let first_sequence = replay
        .bars()
        .first()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "engine snapshot is empty".to_string())?;
    let last_sequence = replay
        .bars()
        .last()
        .map(|bar| bar.value().source_sequence)
        .ok_or_else(|| "engine snapshot is empty".to_string())?;
    DesktopMarketGeneration::try_new(
        snapshot.provider_generation,
        snapshot.publication_generation,
        first_sequence,
        last_sequence,
        replay.bars().to_vec(),
    )
    .map_err(|error| error.to_string())
}

pub(super) fn tail_publication(
    current: MarketPublicationGeneration,
    tail: &ReplayTailUpdate,
) -> MarketPublicationGeneration {
    let sequence = tail.item().value().source_sequence;
    let (mut first, _) = current.sequence_range();
    let retained = match tail.operation() {
        ReplayTailOperation::Revise => current.retained_items(),
        ReplayTailOperation::Append => {
            let retained = current
                .retained_items()
                .saturating_add(1)
                .min(RETAINED_BAR_CAPACITY);
            if retained == RETAINED_BAR_CAPACITY && current.retained_items() == retained {
                first = first.saturating_add(1);
            }
            retained
        }
    };
    MarketPublicationGeneration::from_tail(tail.publication_generation(), retained, first, sequence)
}

pub(super) fn replay_tail_update(update: &SeriesUpdate) -> Result<ReplayTailUpdate, String> {
    let series = update
        .series
        .as_ref()
        .ok_or_else(|| "engine update has no series identity".to_string())?;
    if series.provider != "rithmic" && series.provider != "hyperliquid"
        || series.cadence_value == 0
        || !matches!(
            SeriesCadence::try_from(series.cadence),
            Ok(SeriesCadence::FixedSeconds
                | SeriesCadence::CalendarWeeks
                | SeriesCadence::CalendarMonths)
        )
    {
        return Err(format!(
            "engine {} update identity is invalid",
            provider_display_name(series.provider.as_str())
        ));
    }
    let bar = update
        .bar
        .as_ref()
        .ok_or_else(|| "engine update has no bar".to_string())?;
    let item = provenanced_engine_bar(
        series,
        update.provider_generation,
        update.consumer_id,
        update.generation,
        bar,
        now_unix_nanos(),
    );
    let operation = match SeriesUpdateOperation::try_from(update.operation) {
        Ok(SeriesUpdateOperation::ReviseTail) => ReplayTailOperation::Revise,
        Ok(SeriesUpdateOperation::AppendTail) => ReplayTailOperation::Append,
        Ok(SeriesUpdateOperation::Unspecified) | Err(_) => {
            return Err("engine update operation is invalid".to_string());
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

pub(super) fn provenanced_engine_bar(
    series: &SeriesKey,
    provider_generation: u64,
    consumer_id: u64,
    generation: u64,
    bar: &axiusflow_engine_protocol::MarketBar,
    received: i64,
) -> Provenanced<MarketBar> {
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
                "engine-{provider_generation}-{generation}-{}",
                bar.source_sequence
            ),
            event_time_unix_nanos: exchange,
            publication_time_unix_nanos: received,
            producer: "axiusflow_engine".to_string(),
            schema_version: 1,
            correlation_id: format!("engine-series-{consumer_id}-{generation}"),
            causation_id: String::new(),
            entitlement_revision: series.entitlement_id.clone(),
            session_generation: provider_generation,
            source_id: series.provider.clone(),
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
}

pub(super) fn series_key(
    product: &InstallProviderInstrument,
    interval: ChartInterval,
) -> Result<SeriesKey, String> {
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
    let (cadence, cadence_value) = match interval {
        ChartInterval::Minute1 => (SeriesCadence::FixedSeconds, 60),
        ChartInterval::Minute3 => (SeriesCadence::FixedSeconds, 180),
        ChartInterval::Minute5 => (SeriesCadence::FixedSeconds, 300),
        ChartInterval::Minute15 => (SeriesCadence::FixedSeconds, 900),
        ChartInterval::Minute30 => (SeriesCadence::FixedSeconds, 1_800),
        ChartInterval::Hour1 => (SeriesCadence::FixedSeconds, 3_600),
        ChartInterval::Hour2 => (SeriesCadence::FixedSeconds, 7_200),
        ChartInterval::Hour4 => (SeriesCadence::FixedSeconds, 14_400),
        ChartInterval::Hour8 => (SeriesCadence::FixedSeconds, 28_800),
        ChartInterval::Hour12 => (SeriesCadence::FixedSeconds, 43_200),
        ChartInterval::Day1 => (SeriesCadence::FixedSeconds, 86_400),
        ChartInterval::Week1 => (SeriesCadence::CalendarWeeks, 1),
        ChartInterval::Month1 => (SeriesCadence::CalendarMonths, 1),
        // Hyperliquid serves a native 3-day candle; Rithmic has no Day3
        // series. Tick candles exist on neither public path: Hyperliquid
        // exposes no tick history and the Rithmic test feed prints none.
        ChartInterval::Day3 if product.provider == "hyperliquid" => (SeriesCadence::SessionDays, 3),
        ChartInterval::Tick100 | ChartInterval::Day3 => {
            return Err(format!(
                "{} chart interval is unsupported",
                provider_display_name(product.provider.as_str())
            ));
        }
    };
    Ok(SeriesKey {
        provider: product.provider.clone(),
        instrument_id: product.instrument_id.clone(),
        cadence_value,
        definition_revision: 1,
        entitlement_id: product.entitlement_id.clone(),
        cadence: cadence as i32,
    })
}

pub(super) fn replay_bar_definition(series: &SeriesKey) -> Result<BarDefinition, String> {
    let (cadence_id, interval_seconds, calendar_months) =
        match SeriesCadence::try_from(series.cadence) {
            Ok(SeriesCadence::FixedSeconds) if series.cadence_value > 0 => (
                format!("{}s", series.cadence_value),
                series.cadence_value,
                None,
            ),
            Ok(SeriesCadence::CalendarWeeks) if series.cadence_value > 0 => (
                format!("calendar-weeks:{}", series.cadence_value),
                series
                    .cadence_value
                    .checked_mul(7 * 24 * 60 * 60)
                    .ok_or_else(|| "engine calendar-week cadence overflowed".to_string())?,
                None,
            ),
            Ok(SeriesCadence::CalendarMonths) if series.cadence_value > 0 => (
                format!("calendar-months:{}", series.cadence_value),
                0,
                Some(series.cadence_value),
            ),
            _ => return Err("engine bar definition is invalid".to_string()),
        };
    Ok(BarDefinition {
        definition_id: format!("{}:{}:{cadence_id}", series.provider, series.instrument_id),
        version: series.definition_revision,
        interval_seconds,
        trades_per_bar: None,
        calendar_months,
    })
}
