//! Desktop command access to the single in-process trading owner.

use aeris_contracts::InstallProviderInstrument;
use aeris_instruments::{
    ContractDate, ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
    SessionHours,
};
use aeris_trading_runtime::{
    PlaceOrder, SimulatedMarketObservation, TradingInstrument, TradingService,
};
use std::sync::OnceLock;

static TRADING_SERVICE: OnceLock<TradingService> = OnceLock::new();

/// Installs the process-wide trading owner before any market worker can resolve instruments.
///
/// # Errors
/// Returns an error when an owner was already installed in this process.
pub fn install(service: TradingService) -> Result<(), String> {
    TRADING_SERVICE
        .set(service)
        .map_err(|_| "desktop trading owner is already installed".to_string())
}

/// Returns a cloneable command handle without exposing the owner to UI state.
#[must_use]
pub fn handle() -> Option<TradingService> {
    TRADING_SERVICE.get().cloned()
}

/// Dispatches a market order and its immediate simulated-market observation off the UI thread.
pub fn dispatch_simulated_market_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    cx: &mut gpui::App,
) {
    let Some(service) = handle() else {
        return;
    };
    let Some((command, observation)) = prepare_simulated_market_order(frame, side) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            if service.place_order(command).is_ok()
                && let Some(observation) = observation
            {
                let _ = service.observe_market(observation);
            }
        })
        .detach();
}

/// Prepares a validated local market order and its optional BBO observation.
#[must_use]
pub fn prepare_simulated_market_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
) -> Option<(PlaceOrder, Option<SimulatedMarketObservation>)> {
    let submitted_unix_nanos = now();
    let account_id = aeris_trading::TradingAccountId::try_new("aeris-sim-1").ok()?;
    let instrument_id = InstrumentId::try_new(frame.instrument_id.clone()).ok()?;
    let client_order_id = aeris_trading::ClientOrderId::try_new(format!(
        "ui-{}-{submitted_unix_nanos}",
        match side {
            aeris_trading::OrderSide::Buy => "buy",
            aeris_trading::OrderSide::Sell => "sell",
        }
    ))
    .ok()?;
    let quantity = aeris_trading::FixedPoint::try_new(1, frame.quantity_scale).ok()?;
    let provenance = aeris_trading::TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: frame.provider_id.clone(),
        session_generation: frame.session_generation,
        source_sequence: frame
            .source_watermark
            .max(frame.bbo_source_watermark)
            .max(1),
        observed_unix_nanos: submitted_unix_nanos,
    };
    let command = PlaceOrder {
        client_order_id,
        account_id,
        instrument_id: instrument_id.clone(),
        side,
        order_type: aeris_trading::OrderType::Market,
        time_in_force: aeris_trading::TimeInForce::Day,
        quantity,
        limit_price: None,
        stop_price: None,
        submitted_unix_nanos,
        provenance: provenance.clone(),
    };
    let observation = frame
        .best_bid
        .as_ref()
        .zip(frame.best_ask.as_ref())
        .and_then(|(bid, ask)| {
            Some(SimulatedMarketObservation {
                instrument_id,
                bid: aeris_trading::FixedPoint::try_new(bid.price, frame.price_scale).ok()?,
                ask: aeris_trading::FixedPoint::try_new(ask.price, frame.price_scale).ok()?,
                provenance,
            })
        });
    Some((command, observation))
}

/// Flattens the simulated account using the current best bid and ask off the UI thread.
pub fn flatten_simulated_account(frame: &aeris_market_data::OrderBookFrame, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    let Some((account_id, observation)) = prepare_flatten(frame) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let _ = service.flatten_account(account_id, observation);
        })
        .detach();
}

/// Prepares the simulated account identity and its current BBO observation.
#[must_use]
pub fn prepare_flatten(
    frame: &aeris_market_data::OrderBookFrame,
) -> Option<(aeris_trading::TradingAccountId, SimulatedMarketObservation)> {
    let account_id = aeris_trading::TradingAccountId::try_new("aeris-sim-1").ok()?;
    let (bid, ask) = frame.best_bid.as_ref().zip(frame.best_ask.as_ref())?;
    let instrument_id = InstrumentId::try_new(frame.instrument_id.clone()).ok()?;
    let bid = aeris_trading::FixedPoint::try_new(bid.price, frame.price_scale).ok()?;
    let ask = aeris_trading::FixedPoint::try_new(ask.price, frame.price_scale).ok()?;
    let observation = SimulatedMarketObservation {
        instrument_id,
        bid,
        ask,
        provenance: aeris_trading::TradingProvenance {
            venue_id: "aeris-sim".to_string(),
            provider_id: frame.provider_id.clone(),
            session_generation: frame.session_generation,
            source_sequence: frame
                .source_watermark
                .max(frame.bbo_source_watermark)
                .max(1),
            observed_unix_nanos: now(),
        },
    };
    Some((account_id, observation))
}

#[must_use]
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(1)
}

/// Registers complete provider contract terms on a market background worker.
///
/// Returns `Ok(false)` when the provider did not supply the minimum currency metadata required
/// for trading. Missing values are never guessed from a symbol or venue.
///
/// # Errors
/// Returns an error for malformed provider metadata, runtime overload, or durable-store failure.
pub fn register_provider_instrument_if_running(
    instrument: &InstallProviderInstrument,
) -> Result<bool, String> {
    let Some(service) = TRADING_SERVICE.get() else {
        return Ok(false);
    };
    let Some(metadata) = instrument.contract_metadata.as_deref() else {
        return Ok(false);
    };
    let Some(currency) = metadata.currency.clone() else {
        return Ok(false);
    };
    let price_scale = u8_scale(instrument.price_scale)?;
    let point_value_scale = metadata.point_value_scale.map(u8_scale).transpose()?;
    let tick_size = instrument
        .price_increment
        .map(|units| InstrumentDecimal::try_new(units, price_scale))
        .transpose()
        .map_err(|error| error.to_string())?;
    let point_value = metadata
        .point_value
        .zip(point_value_scale)
        .map(|(units, scale)| InstrumentDecimal::try_new(units, scale))
        .transpose()
        .map_err(|error| error.to_string())?;
    let session_hours = metadata
        .session_hours
        .iter()
        .map(|session| {
            Ok(SessionHours {
                weekday: u8::try_from(session.weekday)
                    .map_err(|_| "provider session weekday is invalid".to_string())?,
                open_seconds: session.open_seconds,
                close_seconds: session.close_seconds,
                timezone: session.timezone.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let contract = ContractMetadata {
        tick_size,
        point_value,
        currency,
        expiry: metadata
            .contract_expiry
            .as_deref()
            .map(parse_contract_date)
            .transpose()?,
        first_notice: metadata
            .first_notice_date
            .as_deref()
            .map(parse_contract_date)
            .transpose()?,
        last_trade: metadata
            .last_trade_date
            .as_deref()
            .map(parse_contract_date)
            .transpose()?,
        session_hours,
        provenance: InstrumentMetadataProvenance {
            provider_id: instrument.provider.clone(),
            provider_symbol: instrument.provider_symbol.clone(),
            session_generation: instrument.session_generation,
        },
    };
    service.register_instrument(TradingInstrument {
        instrument_id: InstrumentId::try_new(instrument.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        price_scale,
        quantity_scale: u8_scale(instrument.quantity_scale)?,
        contract,
    })?;
    Ok(true)
}

fn u8_scale(scale: u32) -> Result<u8, String> {
    let scale =
        u8::try_from(scale).map_err(|_| "provider instrument scale is invalid".to_string())?;
    if scale > 18 {
        return Err("provider instrument scale exceeds 18".to_string());
    }
    Ok(scale)
}

fn parse_contract_date(value: &str) -> Result<ContractDate, String> {
    let mut parts = value.split('-');
    let year = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .ok_or_else(|| "provider contract date is invalid".to_string())?;
    let month = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok())
        .ok_or_else(|| "provider contract date is invalid".to_string())?;
    let day = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok())
        .ok_or_else(|| "provider contract date is invalid".to_string())?;
    if parts.next().is_some() {
        return Err("provider contract date is invalid".to_string());
    }
    let date = ContractDate { year, month, day };
    date.validate().map_err(|error| error.to_string())?;
    Ok(date)
}

#[cfg(test)]
mod tests {
    use super::parse_contract_date;

    #[test]
    fn contract_dates_require_an_exact_day() {
        assert!(parse_contract_date("2026-12-18").is_ok());
        assert!(parse_contract_date("202612").is_err());
        assert!(parse_contract_date("2026-02-30").is_err());
    }
}
