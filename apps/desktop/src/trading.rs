//! Desktop command access to the single in-process trading owner.

use aeris_contracts::InstallProviderInstrument;
use aeris_instruments::{
    ContractDate, ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
    SessionHours,
};
use aeris_trading_runtime::{
    ModifyOrder, PlaceOrder, SimulatedMarketObservation, TradingInstrument, TradingService,
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
    dispatch_simulated_order(
        frame,
        side,
        None,
        1,
        aeris_trading::OrderType::Market,
        aeris_trading::TimeInForce::Day,
        cx,
    );
}

/// Dispatches the selected simulated order-entry instruction off the UI thread.
pub fn dispatch_simulated_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    account_key: Option<String>,
    quantity: u64,
    order_type: aeris_trading::OrderType,
    time_in_force: aeris_trading::TimeInForce,
    cx: &mut gpui::App,
) {
    let Some(service) = handle() else {
        return;
    };
    let Some((command, observation)) = prepare_simulated_order(
        frame,
        side,
        account_key,
        quantity,
        order_type,
        time_in_force,
    ) else {
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

/// Cancels working orders for the selected simulated account off the UI thread.
pub fn cancel_simulated_account(account_key: Option<String>, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    let account =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok());
    cx.background_executor()
        .spawn(async move {
            let _ = service.cancel_all(account);
        })
        .detach();
}

/// Cancels one working simulated order off the UI thread.
pub fn cancel_simulated_order(client_order_key: String, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    let Ok(client_order_id) = aeris_trading::ClientOrderId::try_new(client_order_key) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let _ = service.cancel_order(client_order_id);
        })
        .detach();
}

/// Reprices one working simulated limit order to the current best bid or ask off the UI thread.
pub fn reprice_simulated_order(
    client_order_key: String,
    side: aeris_trading::OrderSide,
    time_in_force: aeris_trading::TimeInForce,
    frame: &aeris_market_data::OrderBookFrame,
    cx: &mut gpui::App,
) {
    let Some(service) = handle() else {
        return;
    };
    let Ok(client_order_id) = aeris_trading::ClientOrderId::try_new(client_order_key) else {
        return;
    };
    let Some(level) = (match side {
        aeris_trading::OrderSide::Buy => frame.best_ask.as_ref(),
        aeris_trading::OrderSide::Sell => frame.best_bid.as_ref(),
    }) else {
        return;
    };
    let Ok(limit_price) = aeris_trading::FixedPoint::try_new(level.price, frame.price_scale) else {
        return;
    };
    let modified_unix_nanos = now();
    let provenance = aeris_trading::TradingProvenance {
        venue_id: "aeris-sim".to_string(),
        provider_id: frame.provider_id.clone(),
        session_generation: frame.session_generation,
        source_sequence: frame
            .source_watermark
            .max(frame.bbo_source_watermark)
            .max(1),
        observed_unix_nanos: modified_unix_nanos,
    };
    let command = ModifyOrder {
        client_order_id,
        time_in_force,
        limit_price: Some(limit_price),
        stop_price: None,
        modified_unix_nanos,
        provenance,
    };
    cx.background_executor()
        .spawn(async move {
            let _ = service.modify_order(command);
        })
        .detach();
}

/// Cancels working orders for every simulated account off the UI thread.
pub fn cancel_simulated_accounts(cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let _ = service.cancel_all(None);
        })
        .detach();
}

/// Locks the selected simulated account off the UI thread.
pub fn kill_simulated_account(account_key: Option<String>, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    let account =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok());
    let locked_at = now();
    cx.background_executor()
        .spawn(async move {
            let _ = service.kill_switch(account, "manual kill switch".to_string(), locked_at);
        })
        .detach();
}

/// Locks every simulated account off the UI thread.
pub fn kill_simulated_accounts(cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    let locked_at = now();
    cx.background_executor()
        .spawn(async move {
            let _ = service.kill_switch(None, "manual global kill switch".to_string(), locked_at);
        })
        .detach();
}

/// Prepares a validated local market order and its optional BBO observation.
#[must_use]
pub fn prepare_simulated_market_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
) -> Option<(PlaceOrder, Option<SimulatedMarketObservation>)> {
    prepare_simulated_order(
        frame,
        side,
        None,
        1,
        aeris_trading::OrderType::Market,
        aeris_trading::TimeInForce::Day,
    )
}

/// Prepares a validated simulated order-entry instruction and its BBO observation.
#[must_use]
pub fn prepare_simulated_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    selected_account_key: Option<String>,
    quantity_units: u64,
    order_type: aeris_trading::OrderType,
    time_in_force: aeris_trading::TimeInForce,
) -> Option<(PlaceOrder, Option<SimulatedMarketObservation>)> {
    let submitted_unix_nanos = now();
    let account_id = selected_account_key
        .and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())
        .or_else(|| aeris_trading::TradingAccountId::try_new("aeris-sim-1").ok())?;
    let instrument_id = InstrumentId::try_new(frame.instrument_id.clone()).ok()?;
    let client_order_id = aeris_trading::ClientOrderId::try_new(format!(
        "ui-{}-{submitted_unix_nanos}",
        match side {
            aeris_trading::OrderSide::Buy => "buy",
            aeris_trading::OrderSide::Sell => "sell",
        }
    ))
    .ok()?;
    let quantity = aeris_trading::FixedPoint::try_new(
        i64::try_from(quantity_units).ok()?,
        frame.quantity_scale,
    )
    .ok()?;
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
    let (bid, ask) = frame.best_bid.as_ref().zip(frame.best_ask.as_ref())?;
    let bid = aeris_trading::FixedPoint::try_new(bid.price, frame.price_scale).ok()?;
    let ask = aeris_trading::FixedPoint::try_new(ask.price, frame.price_scale).ok()?;
    let entry_price = match side {
        aeris_trading::OrderSide::Buy => ask,
        aeris_trading::OrderSide::Sell => bid,
    };
    let (limit_price, stop_price) = match order_type {
        aeris_trading::OrderType::Market => (None, None),
        aeris_trading::OrderType::Limit => (Some(entry_price), None),
        aeris_trading::OrderType::Stop => (None, Some(entry_price)),
        aeris_trading::OrderType::StopLimit => (Some(entry_price), Some(entry_price)),
    };
    let command = PlaceOrder {
        client_order_id,
        account_id,
        instrument_id: instrument_id.clone(),
        side,
        order_type,
        time_in_force,
        quantity,
        limit_price,
        stop_price,
        submitted_unix_nanos,
        provenance: provenance.clone(),
    };
    let observation = Some(SimulatedMarketObservation {
        instrument_id,
        bid,
        ask,
        provenance,
    });
    Some((command, observation))
}

/// Flattens the simulated account using the current best bid and ask off the UI thread.
pub fn flatten_simulated_account(frame: &aeris_market_data::OrderBookFrame, cx: &mut gpui::App) {
    flatten_simulated_account_for(frame, None, cx);
}

/// Flattens one selected simulated account using the current BBO off the UI thread.
pub fn flatten_simulated_account_for(
    frame: &aeris_market_data::OrderBookFrame,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let Some(service) = handle() else {
        return;
    };
    let Some((account_id, observation)) = prepare_flatten_for(frame, account_key) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let _ = service.flatten_account(account_id, observation);
        })
        .detach();
}

/// Flattens every simulated account using the current best bid and ask off the UI thread.
pub fn flatten_simulated_accounts(frame: &aeris_market_data::OrderBookFrame, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        return;
    };
    let Some((_, observation)) = prepare_flatten(frame) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let _ = service.flatten_all(observation);
        })
        .detach();
}

/// Prepares the simulated account identity and its current BBO observation.
#[must_use]
pub fn prepare_flatten(
    frame: &aeris_market_data::OrderBookFrame,
) -> Option<(aeris_trading::TradingAccountId, SimulatedMarketObservation)> {
    prepare_flatten_for(frame, None)
}

/// Prepares a selected simulated account and current BBO observation.
#[must_use]
pub fn prepare_flatten_for(
    frame: &aeris_market_data::OrderBookFrame,
    selected_account_key: Option<String>,
) -> Option<(aeris_trading::TradingAccountId, SimulatedMarketObservation)> {
    let account_id = selected_account_key
        .and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())
        .or_else(|| aeris_trading::TradingAccountId::try_new("aeris-sim-1").ok())?;
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
    use super::{parse_contract_date, prepare_simulated_order};

    fn order_book_frame() -> aeris_market_data::OrderBookFrame {
        let level = |price| aeris_market_data::OrderBookColumnLevel {
            price,
            quantity: 1,
            order_count: Some(1),
            price_text: price.to_string(),
            quantity_text: "1".to_string(),
            traded_volume: 0,
            traded_volume_text: "0".to_string(),
            relative_size_bps: 10_000,
        };
        aeris_market_data::OrderBookFrame {
            provider_id: "test-provider".to_string(),
            instrument_id: "test:instrument".to_string(),
            entitlement_id: "test-entitlement".to_string(),
            session_generation: 1,
            selection_generation: 1,
            revision: 1,
            source_watermark: 7,
            bbo_source_watermark: 7,
            state: aeris_market_data::OrderBookState::Ready,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(1),
            best_bid: Some(level(10_000)),
            best_ask: Some(level(10_001)),
            traded_volumes: std::collections::BTreeMap::new(),
            trade_source_watermark: 7,
            rows: Vec::new(),
        }
    }

    #[test]
    fn contract_dates_require_an_exact_day() {
        assert!(parse_contract_date("2026-12-18").is_ok());
        assert!(parse_contract_date("202612").is_err());
        assert!(parse_contract_date("2026-02-30").is_err());
    }

    #[test]
    fn order_entry_preserves_selected_account_and_execution_instructions() {
        let account_id =
            aeris_trading::TradingAccountId::try_new("aeris-sim-2").expect("account id");
        let (order, observation) = prepare_simulated_order(
            &order_book_frame(),
            aeris_trading::OrderSide::Sell,
            Some(account_id.as_str().to_string()),
            5,
            aeris_trading::OrderType::StopLimit,
            aeris_trading::TimeInForce::GoodTillCancelled,
        )
        .expect("order command");
        assert_eq!(order.account_id, account_id);
        assert_eq!(order.quantity.units(), 5);
        assert_eq!(order.order_type, aeris_trading::OrderType::StopLimit);
        assert_eq!(
            order.time_in_force,
            aeris_trading::TimeInForce::GoodTillCancelled
        );
        assert_eq!(
            order.limit_price.map(aeris_trading::FixedPoint::units),
            Some(10_000)
        );
        assert_eq!(
            order.stop_price.map(aeris_trading::FixedPoint::units),
            Some(10_000)
        );
        let observation = observation.expect("BBO observation");
        assert_eq!(observation.bid.units(), 10_000);
        assert_eq!(observation.ask.units(), 10_001);
    }
}
