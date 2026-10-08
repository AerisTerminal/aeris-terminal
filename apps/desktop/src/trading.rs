//! Desktop command access to the single in-process trading owner.

use aeris_contracts::{InstallProviderInstrument, ProviderContractMetadata};
use aeris_instruments::{
    ContractDate, ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
    SessionHours,
};
use aeris_trading_runtime::{
    CreatePracticeAccount, ModifyOrder, PlaceBracket, PlaceOrder, SimulatedMarketObservation,
    TradeCopierConfig, TradingInstrument, TradingService,
};
use std::sync::{Mutex, OnceLock};

static TRADING_SERVICE: OnceLock<TradingService> = OnceLock::new();
static TRADING_FEEDBACK: OnceLock<Mutex<TradingFeedbackState>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingCommandFeedback {
    pub revision: u64,
    pub message: String,
    pub is_error: bool,
}

#[derive(Default)]
struct TradingFeedbackState {
    revision: u64,
    latest: Option<TradingCommandFeedback>,
}

fn record_feedback(result: Result<String, String>) {
    let feedback = TRADING_FEEDBACK.get_or_init(|| Mutex::new(TradingFeedbackState::default()));
    let mut feedback = feedback
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    feedback.revision = feedback.revision.saturating_add(1).max(1);
    let revision = feedback.revision;
    feedback.latest = Some(match result {
        Ok(message) => TradingCommandFeedback {
            revision,
            message,
            is_error: false,
        },
        Err(message) => TradingCommandFeedback {
            revision,
            message,
            is_error: true,
        },
    });
}

/// Records a command result for the order-entry status line and reports whether it succeeded.
pub fn record_outcome<T>(result: Result<T, String>, success: &str) -> bool {
    let accepted = result.is_ok();
    record_feedback(result.map(|_| success.to_string()));
    accepted
}

#[must_use]
pub fn latest_feedback() -> Option<TradingCommandFeedback> {
    TRADING_FEEDBACK.get().and_then(|feedback| {
        feedback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest
            .clone()
    })
}

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

/// Creates a durable user-named practice account off the UI thread.
pub fn create_practice_account(name: String, equity: &str, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let starting_equity = match parse_usd_equity(equity) {
        Ok(value) => value,
        Err(error) => {
            record_feedback(Err(error));
            return;
        }
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .create_practice_account(CreatePracticeAccount {
                        display_name: name,
                        starting_equity,
                    })
                    .map(|account| format!("Created {}", account.display_name)),
            );
        })
        .detach();
}

/// Permanently removes one flat simulated practice account off the UI thread.
pub fn delete_practice_account(account_key: String, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let account_id = match aeris_trading::TradingAccountId::try_new(account_key) {
        Ok(account_id) => account_id,
        Err(error) => {
            record_feedback(Err(error.to_string()));
            return;
        }
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .delete_practice_account(account_id)
                    .map(|account| format!("Deleted {}", account.display_name)),
            );
        })
        .detach();
}

fn parse_usd_equity(value: &str) -> Result<aeris_trading::FixedPoint, String> {
    let value = value.trim().trim_start_matches('$').replace(',', "");
    let (whole, fraction) = value.split_once('.').unwrap_or((&value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(
            "Starting equity must be a positive USD amount with at most two decimals".to_string(),
        );
    }
    let whole = whole
        .parse::<i64>()
        .map_err(|_| "Starting equity is too large".to_string())?;
    let fractional = match fraction.len() {
        0 => 0,
        1 => {
            fraction
                .parse::<i64>()
                .map_err(|_| "Starting equity is invalid")?
                * 10
        }
        _ => fraction
            .parse::<i64>()
            .map_err(|_| "Starting equity is invalid")?,
    };
    let units = whole
        .checked_mul(100)
        .and_then(|units| units.checked_add(fractional))
        .filter(|units| *units > 0)
        .ok_or_else(|| {
            "Starting equity must be positive and within the supported range".to_string()
        })?;
    aeris_trading::FixedPoint::try_new(units, 2).map_err(|error| error.to_string())
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
    if order_type_requires_price(order_type) {
        record_feedback(Err(STOP_PRICE_REQUIRED.to_string()));
        return;
    }
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
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
        record_feedback(Err(
            "Practice order requires a current bid, ask, and valid instrument".to_string(),
        ));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let result = service
                .place_order(command)
                .and_then(|order| placement_outcome(&service, &order.client_order_id, observation));
            record_feedback(result);
        })
        .detach();
}

/// User-selected order instruction with an optional runtime-owned bracket template.
pub struct SimulatedOrderSelection {
    pub account_key: Option<String>,
    pub quantity: u64,
    pub order_type: aeris_trading::OrderType,
    pub time_in_force: aeris_trading::TimeInForce,
    pub template_id: Option<String>,
}

/// Dispatches a plain or bracketed entry selected by the order-entry panel off the UI thread.
pub fn dispatch_simulated_selected_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    selection: SimulatedOrderSelection,
    cx: &mut gpui::App,
) {
    if order_type_requires_price(selection.order_type) {
        record_feedback(Err(STOP_PRICE_REQUIRED.to_string()));
        return;
    }
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Some((entry, observation)) = prepare_simulated_order(
        frame,
        side,
        selection.account_key,
        selection.quantity,
        selection.order_type,
        selection.time_in_force,
    ) else {
        record_feedback(Err(
            "Practice order requires a current bid, ask, and valid instrument".to_string(),
        ));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let accepted = if let Some(template_id) = selection.template_id {
                service
                    .place_bracket(PlaceBracket { entry, template_id })
                    .map(|bracket| bracket.entry_client_order_id)
            } else {
                service
                    .place_order(entry)
                    .map(|order| order.client_order_id)
            };
            let result = accepted.and_then(|client_order_id| {
                placement_outcome(&service, &client_order_id, observation)
            });
            record_feedback(result);
        })
        .detach();
}

const STOP_PRICE_REQUIRED: &str = "Click a ladder price to place a stop order";

fn order_type_requires_price(order_type: aeris_trading::OrderType) -> bool {
    matches!(
        order_type,
        aeris_trading::OrderType::Stop | aeris_trading::OrderType::StopLimit
    )
}

/// Returns the working-order outcome after applying the submission-time BBO.
fn placement_outcome(
    service: &TradingService,
    client_order_id: &aeris_trading::ClientOrderId,
    observation: Option<SimulatedMarketObservation>,
) -> Result<String, String> {
    let fills = observation
        .map(|observation| service.observe_market(observation))
        .transpose()?
        .unwrap_or_default();
    Ok(if fills.is_empty() {
        format!("Practice order {} is working", client_order_id.as_str())
    } else {
        format!("Practice order filled · {} fill(s)", fills.len())
    })
}

/// Applies an exact price to the price field(s) that the order type uses.
fn price_fields(
    order_type: aeris_trading::OrderType,
    price: aeris_trading::FixedPoint,
) -> (
    Option<aeris_trading::FixedPoint>,
    Option<aeris_trading::FixedPoint>,
) {
    match order_type {
        aeris_trading::OrderType::Market => (None, None),
        aeris_trading::OrderType::Limit => (Some(price), None),
        aeris_trading::OrderType::Stop => (None, Some(price)),
        aeris_trading::OrderType::StopLimit => (Some(price), Some(price)),
    }
}

/// Exact-price ladder instruction for one simulated order.
pub struct SimulatedPricedOrder {
    pub side: aeris_trading::OrderSide,
    pub order_type: aeris_trading::OrderType,
    pub account_key: Option<String>,
    pub quantity: u64,
    pub time_in_force: aeris_trading::TimeInForce,
    pub price_units: i64,
}

/// Dispatches a simulated limit or stop order at the clicked order-book price off the UI thread.
pub fn dispatch_simulated_order_at_price(
    frame: &aeris_market_data::OrderBookFrame,
    order: SimulatedPricedOrder,
    cx: &mut gpui::App,
) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Some((mut command, observation)) = prepare_simulated_order(
        frame,
        order.side,
        order.account_key,
        order.quantity,
        order.order_type,
        order.time_in_force,
    ) else {
        record_feedback(Err(
            "Practice order requires a current bid, ask, and valid instrument".to_string(),
        ));
        return;
    };
    let Ok(price) = aeris_trading::FixedPoint::try_new(order.price_units, frame.price_scale) else {
        record_feedback(Err("Practice order price is invalid".to_string()));
        return;
    };
    (command.limit_price, command.stop_price) = price_fields(order.order_type, price);
    cx.background_executor()
        .spawn(async move {
            let result = service
                .place_order(command)
                .and_then(|order| placement_outcome(&service, &order.client_order_id, observation));
            record_feedback(result);
        })
        .detach();
}

/// Cancels working orders for the selected simulated account off the UI thread.
pub fn cancel_simulated_account(account_key: Option<String>, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let account =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok());
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .cancel_all(account)
                    .map(|orders| format!("Cancelled {} practice order(s)", orders.len())),
            );
        })
        .detach();
}

/// Cancels one working simulated order off the UI thread.
pub fn cancel_simulated_order(client_order_key: String, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Ok(client_order_id) = aeris_trading::ClientOrderId::try_new(client_order_key) else {
        record_feedback(Err("Practice order identifier is invalid".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_outcome(
                service.cancel_order(client_order_id),
                "Practice order cancelled",
            );
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
    let Ok(client_order_id) = aeris_trading::ClientOrderId::try_new(client_order_key) else {
        record_feedback(Err("Practice order identifier is invalid".to_string()));
        return;
    };
    let Some(level) = (match side {
        aeris_trading::OrderSide::Buy => frame.best_ask.as_ref(),
        aeris_trading::OrderSide::Sell => frame.best_bid.as_ref(),
    }) else {
        record_feedback(Err("A current bid or ask is unavailable".to_string()));
        return;
    };
    let Ok(limit_price) = aeris_trading::FixedPoint::try_new(level.price, frame.price_scale) else {
        record_feedback(Err("Practice limit price is invalid".to_string()));
        return;
    };
    modify_simulated_order_at_price(
        client_order_id,
        aeris_trading::OrderType::Limit,
        time_in_force,
        frame,
        limit_price,
        cx,
    );
}

/// Moves one working simulated limit or stop order to an exact ladder price off the UI thread.
pub fn modify_simulated_order_at_price(
    client_order_id: aeris_trading::ClientOrderId,
    order_type: aeris_trading::OrderType,
    time_in_force: aeris_trading::TimeInForce,
    frame: &aeris_market_data::OrderBookFrame,
    price: aeris_trading::FixedPoint,
    cx: &mut gpui::App,
) {
    if order_type == aeris_trading::OrderType::Market {
        record_feedback(Err("Market orders have no price to modify".to_string()));
        return;
    }
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
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
    let (limit_price, stop_price) = price_fields(order_type, price);
    let command = ModifyOrder {
        client_order_id,
        time_in_force,
        limit_price,
        stop_price,
        modified_unix_nanos,
        provenance,
    };
    cx.background_executor()
        .spawn(async move {
            record_outcome(service.modify_order(command), "Practice order modified");
        })
        .detach();
}

/// Cancels working orders for every simulated account off the UI thread.
pub fn cancel_simulated_accounts(cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .cancel_all(None)
                    .map(|orders| format!("Cancelled {} practice order(s)", orders.len())),
            );
        })
        .detach();
}

/// Locks the selected simulated account off the UI thread.
pub fn kill_simulated_account(account_key: Option<String>, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let account =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok());
    let locked_at = now();
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .kill_switch(account, "manual kill switch".to_string(), locked_at)
                    .map(|count| format!("Locked {count} practice account(s)")),
            );
        })
        .detach();
}

/// Clears the durable manual risk lock for one selected simulated account.
pub fn unlock_simulated_account(account_key: Option<String>, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Some(account) =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())
    else {
        record_feedback(Err("Select a practice account to unlock".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_outcome(service.unlock_account(account), "Practice account unlocked");
        })
        .detach();
}

/// Locks every simulated account off the UI thread.
pub fn kill_simulated_accounts(cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let locked_at = now();
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .kill_switch(None, "manual global kill switch".to_string(), locked_at)
                    .map(|count| format!("Locked {count} practice account(s)")),
            );
        })
        .detach();
}

/// Replaces one runtime-owned trade-copier configuration off the UI thread.
pub fn register_trade_copier(config: TradeCopierConfig, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_outcome(
                service.register_trade_copier(config),
                "Practice trade copier updated",
            );
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
    if frame.state != aeris_market_data::OrderBookState::Ready {
        return None;
    }
    let submitted_unix_nanos = now();
    let account_id = selected_account_key
        .and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())?;
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
        display_quantity_units(quantity_units, frame.quantity_scale)?,
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

fn display_quantity_units(quantity: u64, scale: u8) -> Option<i64> {
    let multiplier = 10_u64.checked_pow(u32::from(scale))?;
    quantity
        .checked_mul(multiplier)
        .and_then(|units| i64::try_from(units).ok())
}

/// Publishes one canonical book BBO to the simulated venue from a market worker thread.
///
/// The trading owner applies each provider revision once per instrument, so several panes
/// publishing the same book do not multiply simulated-venue work. The bounded command queue
/// never blocks the market worker; an overflow is reported and the next revision supersedes it.
pub fn publish_simulated_market_observation(frame: &aeris_market_data::OrderBookFrame) {
    let Some(service) = TRADING_SERVICE.get() else {
        return;
    };
    let Some(observation) = simulated_market_observation(frame) else {
        return;
    };
    if let Err(error) = service.publish_market_observation(observation) {
        record_feedback(Err(format!("Practice market update failed: {error}")));
    }
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
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Some((account_id, observation)) = prepare_flatten_for(frame, account_key) else {
        record_feedback(Err(
            "Flatten requires a selected account and current bid and ask".to_string(),
        ));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .flatten_account(account_id, observation)
                    .map(|outcome| {
                        format!(
                            "Flattened practice account · {} fill(s)",
                            outcome.fills.len()
                        )
                    }),
            );
        })
        .detach();
}

/// Closes and reverses the selected practice position at the current BBO.
pub fn reverse_simulated_position(
    frame: &aeris_market_data::OrderBookFrame,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Some((account_id, observation)) = prepare_flatten_for(frame, account_key) else {
        record_feedback(Err(
            "Reverse requires a selected account and current bid and ask".to_string(),
        ));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(
                service
                    .reverse_position(account_id, observation)
                    .map(|outcome| {
                        format!(
                            "Reversed practice position · {} fill(s)",
                            outcome.fills.len()
                        )
                    }),
            );
        })
        .detach();
}

/// Flattens every simulated account using the current best bid and ask off the UI thread.
pub fn flatten_simulated_accounts(frame: &aeris_market_data::OrderBookFrame, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let Some(observation) = simulated_market_observation(frame) else {
        record_feedback(Err("Flatten requires a current bid and ask".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(service.flatten_all(observation).map(|outcome| {
                format!(
                    "Flattened all practice accounts · {} fill(s)",
                    outcome.fills.len()
                )
            }));
        })
        .detach();
}

/// Prepares a selected simulated account and current BBO observation.
#[must_use]
pub fn prepare_flatten_for(
    frame: &aeris_market_data::OrderBookFrame,
    selected_account_key: Option<String>,
) -> Option<(aeris_trading::TradingAccountId, SimulatedMarketObservation)> {
    let account_id = selected_account_key
        .and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())?;
    let observation = simulated_market_observation(frame)?;
    Some((account_id, observation))
}

/// Converts one current order-book BBO into the practice venue's canonical market observation.
/// This path intentionally does not require a selected account so global market publication and
/// Flatten All cannot be disabled by account-selection state.
#[must_use]
pub fn simulated_market_observation(
    frame: &aeris_market_data::OrderBookFrame,
) -> Option<SimulatedMarketObservation> {
    if frame.state != aeris_market_data::OrderBookState::Ready {
        return None;
    }
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
    Some(observation)
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
/// # Errors
/// Returns an error for incomplete or malformed provider metadata, runtime overload, or durable-store failure.
pub fn register_provider_instrument(instrument: &InstallProviderInstrument) -> Result<(), String> {
    let service = TRADING_SERVICE
        .get()
        .ok_or_else(|| "Practice trading owner is unavailable".to_string())?;
    service.register_instrument(trading_instrument_from_install(instrument)?)?;
    record_feedback(Ok(format!(
        "Practice trading ready for {}",
        instrument.display_symbol
    )));
    Ok(())
}

fn trading_instrument_from_install(
    instrument: &InstallProviderInstrument,
) -> Result<TradingInstrument, String> {
    let (metadata, currency) = complete_contract_terms(instrument)?;
    let price_scale = u8_scale(instrument.price_scale)?;
    let quantity_scale = u8_scale(instrument.quantity_scale)?;
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
        order_quantity_increment: metadata
            .order_quantity_increment
            .map(|units| InstrumentDecimal::try_new(units, quantity_scale))
            .transpose()
            .map_err(|error| error.to_string())?,
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
            display_symbol: instrument.display_symbol.clone(),
            session_generation: instrument.session_generation,
        },
    };
    Ok(TradingInstrument {
        instrument_id: InstrumentId::try_new(instrument.instrument_id.clone())
            .map_err(|error| error.to_string())?,
        price_scale,
        quantity_scale,
        contract,
    })
}

fn complete_contract_terms(
    instrument: &InstallProviderInstrument,
) -> Result<(&ProviderContractMetadata, String), String> {
    let unavailable = |detail: &str| {
        format!(
            "Practice trading unavailable for {}: {detail}",
            instrument.display_symbol
        )
    };
    let metadata = instrument
        .contract_metadata
        .as_deref()
        .ok_or_else(|| unavailable("contract metadata missing"))?;
    let currency = metadata
        .currency
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| unavailable("contract currency missing"))?;
    if metadata.point_value.is_none() || metadata.point_value_scale.is_none() {
        return Err(unavailable("contract point value missing"));
    }
    Ok((metadata, currency))
}

/// Shows incomplete provider contract terms in the order-entry status while
/// allowing the chart to continue loading.
pub fn report_practice_registration_error(error: String) {
    record_feedback(Err(error));
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
    use super::{
        complete_contract_terms, parse_contract_date, parse_usd_equity, prepare_simulated_order,
        simulated_market_observation, trading_instrument_from_install,
    };

    #[test]
    fn incomplete_contract_terms_name_the_missing_field_in_order_entry_copy() {
        let mut instrument = aeris_contracts::InstallProviderInstrument {
            provider: "tastytrade".into(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "tastytrade:Future:/ESZ6".into(),
            provider_symbol: "/ESZ26:XCME".into(),
            display_symbol: "ESZ6".into(),
            venue_id: "CME".into(),
            price_scale: 8,
            quantity_scale: 8,
            entitlement_id: "tastytrade-market".into(),
            price_increment: Some(25_000_000),
            contract_metadata: Some(Box::new(aeris_contracts::ProviderContractMetadata {
                point_value: Some(5_000_000_000),
                point_value_scale: Some(8),
                currency: None,
                contract_expiry: None,
                first_notice_date: None,
                last_trade_date: None,
                session_hours: Vec::new(),
                order_quantity_increment: Some(100_000_000),
            })),
        };
        assert_eq!(
            complete_contract_terms(&instrument).unwrap_err(),
            "Practice trading unavailable for ESZ6: contract currency missing"
        );
        instrument.contract_metadata.as_mut().unwrap().currency = Some("USD".into());
        assert_eq!(complete_contract_terms(&instrument).unwrap().1, "USD");
        let registered =
            trading_instrument_from_install(&instrument).expect("exact provider terms");
        assert_eq!(
            registered.contract.point_value.unwrap().units(),
            5_000_000_000
        );
        assert_eq!(registered.contract.tick_size.unwrap().units(), 25_000_000);
        assert_eq!(registered.contract.currency, "USD");
        assert_eq!(
            registered
                .contract
                .order_quantity_increment
                .unwrap()
                .units(),
            100_000_000
        );

        instrument.instrument_id = "tastytrade:Equity:AAPL".into();
        instrument.provider_symbol = "AAPL".into();
        instrument.display_symbol = "AAPL".into();
        instrument.price_increment = Some(1_000_000);
        let terms = instrument.contract_metadata.as_mut().unwrap();
        terms.point_value = Some(1);
        terms.point_value_scale = Some(0);
        let registered = trading_instrument_from_install(&instrument).expect("equity terms");
        assert_eq!(registered.contract.point_value.unwrap().units(), 1);
        assert_eq!(registered.contract.point_value.unwrap().scale(), 0);
        assert_eq!(registered.contract.tick_size.unwrap().units(), 1_000_000);
        assert_eq!(registered.contract.currency, "USD");
    }

    fn order_book_frame() -> aeris_market_data::OrderBookFrame {
        let level = |price| aeris_market_data::OrderBookColumnLevel {
            price,
            quantity: Some(1),
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
            top_of_book_only: false,
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

    #[test]
    fn order_entry_converts_display_quantity_to_provider_fixed_point_units() {
        let mut frame = order_book_frame();
        frame.quantity_scale = 3;
        let (order, _) = prepare_simulated_order(
            &frame,
            aeris_trading::OrderSide::Buy,
            Some("aeris-practice-1".to_string()),
            5,
            aeris_trading::OrderType::Market,
            aeris_trading::TimeInForce::Day,
        )
        .expect("scaled order command");
        assert_eq!(order.quantity.units(), 5_000);
        assert_eq!(order.quantity.scale(), 3);
    }

    #[test]
    fn global_practice_market_observation_does_not_require_selected_account() {
        let observation =
            simulated_market_observation(&order_book_frame()).expect("current BBO observation");
        assert_eq!(observation.instrument_id.as_str(), "test:instrument");
        assert_eq!(observation.bid.units(), 10_000);
        assert_eq!(observation.ask.units(), 10_001);
    }

    #[test]
    fn recovering_book_cannot_fill_a_practice_order() {
        let mut frame = order_book_frame();
        frame.state = aeris_market_data::OrderBookState::Stale;
        assert!(simulated_market_observation(&frame).is_none());
        assert!(
            prepare_simulated_order(
                &frame,
                aeris_trading::OrderSide::Buy,
                Some("aeris-practice-1".into()),
                1,
                aeris_trading::OrderType::Market,
                aeris_trading::TimeInForce::Day,
            )
            .is_none()
        );
    }

    #[test]
    fn practice_equity_accepts_usd_and_rejects_ambiguous_values() {
        let equity = parse_usd_equity("$50,000.25").expect("valid starting equity");
        assert_eq!(equity.units(), 5_000_025);
        assert_eq!(equity.scale(), 2);

        for invalid in ["", "0", "-1", "10.001", "ten"] {
            assert!(parse_usd_equity(invalid).is_err(), "accepted {invalid}");
        }
    }
}
