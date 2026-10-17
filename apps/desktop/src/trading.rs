//! Desktop command access to the single in-process trading owner.

use aeris_instruments::InstrumentId;
use aeris_trading_runtime::{
    CreatePracticeAccount, ModifyOrder, PlaceBracket, PlaceOrder, TradeCopierConfig, TradingService,
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
/// Feedback for a flatten, reverse or close. Simulated fills are immediate; broker closes
/// are requests whose fills the broker confirms later, so they are reported as requested,
/// never as filled. Anything that could not be requested makes the outcome an error.
///
/// # Errors
/// Returns the command's error, or the reported work with what could not be requested.
pub fn flatten_feedback(
    action: &str,
    result: Result<aeris_trading_runtime::FlattenOutcome, String>,
) -> Result<String, String> {
    let outcome = result?;
    let mut parts = vec![action.to_string()];
    if !outcome.fills.is_empty() {
        parts.push(format!("{} fill(s)", outcome.fills.len()));
    }
    if !outcome.pending_close_requests.is_empty() {
        parts.push(format!(
            "{} cTrader close request(s) sent",
            outcome.pending_close_requests.len()
        ));
    }
    if !outcome.unpriced.is_empty() {
        parts.push(format!(
            "{} position(s) lack a verified quote and keep their protection",
            outcome.unpriced.len()
        ));
    }
    let incomplete = outcome.incomplete;
    parts.extend(incomplete.clone());
    let message = parts.join(" · ");
    if incomplete.is_some() {
        Err(message)
    } else {
        Ok(message)
    }
}

/// Renders one kill-switch outcome for the feedback line: locks, flatten fills,
/// cancellations and anything left incomplete.
///
/// # Errors
/// Returns the command's error, or the reported work with what could not be requested.
pub fn kill_feedback(
    action: &str,
    result: Result<aeris_trading_runtime::EmergencyOutcome, String>,
) -> Result<String, String> {
    let outcome = result?;
    let mut parts = vec![format!(
        "{action}: locked {} account(s)",
        outcome.accounts_locked
    )];
    if !outcome.fills.is_empty() {
        parts.push(format!("{} flatten fill(s)", outcome.fills.len()));
    }
    if !outcome.cancelled.is_empty() {
        parts.push(format!("{} order(s) cancelled", outcome.cancelled.len()));
    }
    if !outcome.pending_close_requests.is_empty() {
        parts.push(format!(
            "{} cTrader close request(s) sent",
            outcome.pending_close_requests.len()
        ));
    }
    if !outcome.unpriced.is_empty() {
        parts.push(format!(
            "{} position(s) lack a verified quote and keep their protection",
            outcome.unpriced.len()
        ));
    }
    let incomplete = outcome.incomplete;
    parts.extend(incomplete.clone());
    let message = parts.join(" · ");
    if incomplete.is_some() {
        Err(message)
    } else {
        Ok(message)
    }
}

pub fn record_outcome<T>(result: Result<T, String>, success: &str) -> bool {
    record_result(result.map(|_| success.to_string()))
}

/// Shows a message in the trading status row for something the desktop decided rather than a
/// command result, such as a shortcut refused while keyboard trading is off.
pub fn record_notice(notice: Result<String, String>) {
    record_feedback(notice);
}

/// Records feedback whose message the command already composed; returns whether it
/// succeeded.
#[must_use]
pub fn record_result(result: Result<String, String>) -> bool {
    let accepted = result.is_ok();
    record_feedback(result);
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

/// Dispatches a market order through the owner quote book off the UI thread.
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
    let Some(command) = prepare_simulated_order(
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
                .place_order_from_market(command, true)
                .map(|order| placement_outcome(&order));
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
    let Some(entry) = prepare_simulated_order(
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
                    .place_bracket_from_market(PlaceBracket { entry, template_id })
                    .map(|placement| placement.entry_client_order_id().clone())
            } else {
                service
                    .place_order_from_market(entry, true)
                    .map(|order| order.client_order_id)
            };
            let result = accepted.map(|client_order_id| {
                format!("Practice order {} accepted", client_order_id.as_str())
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

fn placement_outcome(order: &aeris_trading::Order) -> String {
    format!("Practice order {} accepted", order.client_order_id.as_str())
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
        aeris_trading::OrderType::Limit | aeris_trading::OrderType::RestingAt => {
            (Some(price), None)
        }
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
    let Some(mut command) = prepare_simulated_order(
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
                .place_order_from_market(command, false)
                .map(|order| placement_outcome(&order));
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

/// Requests closing one whole broker position off the UI thread. The position stays until the
/// broker reports it closed.
pub fn close_broker_position(account_key: String, broker_position_id: String, cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Trading is unavailable".to_string()));
        return;
    };
    let Ok(account_id) = aeris_trading::TradingAccountId::try_new(account_key) else {
        record_feedback(Err("Broker account identifier is invalid".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_outcome(
                service.close_broker_position(account_id, broker_position_id),
                "Close sent to the broker",
            );
        })
        .detach();
}

/// Reprices one working simulated limit order to the current best bid or ask off the UI thread.
pub fn reprice_simulated_order(
    client_order_key: String,
    expected_revision: u32,
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
        expected_revision,
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
    expected_revision: u32,
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
        source_sequence: 1, // Placeholder; the trading owner stamps the current quote.
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
            record_outcome(
                service
                    .modify_order_from_market_checked(
                        aeris_trading_runtime::RevisionedModifyOrder {
                            order: command,
                            expected_revision: Some(expected_revision),
                        },
                    )
                    .map_err(|error| error.to_string()),
                "Practice order modified",
            );
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

/// Locks the selected account, cancels its non-protective orders and flattens every priced position, off the UI thread.
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
            record_feedback(kill_feedback(
                "Kill switch",
                service.kill_switch(account, "manual kill switch".to_string(), locked_at),
            ));
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

/// Locks every tradable account, cancels non-protective orders and flattens every priced position, off the UI thread.
pub fn kill_simulated_accounts(cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    let locked_at = now();
    cx.background_executor()
        .spawn(async move {
            record_feedback(kill_feedback(
                "Global kill switch",
                service.kill_switch(None, "manual global kill switch".to_string(), locked_at),
            ));
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
) -> Option<PlaceOrder> {
    prepare_simulated_order(
        frame,
        side,
        None,
        1,
        aeris_trading::OrderType::Market,
        aeris_trading::TimeInForce::Day,
    )
}

/// Prepares an identity-bearing order instruction; the owner prices it from its quote book.
#[must_use]
pub fn prepare_simulated_order(
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    selected_account_key: Option<String>,
    quantity_units: u64,
    order_type: aeris_trading::OrderType,
    time_in_force: aeris_trading::TimeInForce,
) -> Option<PlaceOrder> {
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
        source_sequence: 1, // Placeholder; the trading owner stamps the current quote.
        observed_unix_nanos: submitted_unix_nanos,
    };
    let (limit_price, stop_price) = (None, None);
    let command = PlaceOrder {
        client_order_id,
        account_id,
        instrument_id,
        side,
        order_type,
        time_in_force,
        quantity,
        limit_price,
        stop_price,
        submitted_unix_nanos,
        provenance,
    };
    Some(command)
}

fn display_quantity_units(quantity: u64, scale: u8) -> Option<i64> {
    let multiplier = 10_u64.checked_pow(u32::from(scale))?;
    quantity
        .checked_mul(multiplier)
        .and_then(|units| i64::try_from(units).ok())
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
    let Some((account_id, instrument)) = prepare_flatten_for(frame, account_key) else {
        record_feedback(Err(
            "Flatten requires a selected account and current bid and ask".to_string(),
        ));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(flatten_feedback(
                "Flattened",
                service.flatten(aeris_trading_runtime::FlattenScope {
                    account: Some(account_id),
                    instrument: Some(instrument),
                }),
            ));
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
    let Some((account_id, instrument)) = prepare_flatten_for(frame, account_key) else {
        record_feedback(Err(
            "Reverse requires a selected account and current bid and ask".to_string(),
        ));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(flatten_feedback(
                "Reversed",
                service.reverse_position_at(account_id, instrument),
            ));
        })
        .detach();
}

/// Flattens every simulated account at the runtime's verified quotes off the UI thread.
pub fn flatten_simulated_accounts(cx: &mut gpui::App) {
    let Some(service) = handle() else {
        record_feedback(Err("Practice trading is unavailable".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_feedback(flatten_feedback(
                "Flattened all accounts",
                service.flatten(aeris_trading_runtime::FlattenScope::default()),
            ));
        })
        .detach();
}

/// Prepares the selected account and instrument identities.
#[must_use]
pub fn prepare_flatten_for(
    frame: &aeris_market_data::OrderBookFrame,
    selected_account_key: Option<String>,
) -> Option<(aeris_trading::TradingAccountId, InstrumentId)> {
    let account_id = selected_account_key
        .and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())?;
    Some((
        account_id,
        InstrumentId::try_new(frame.instrument_id.clone()).ok()?,
    ))
}

#[must_use]
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::{parse_usd_equity, prepare_simulated_order};

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
    fn order_entry_preserves_selected_account_and_execution_instructions() {
        let account_id =
            aeris_trading::TradingAccountId::try_new("aeris-sim-2").expect("account id");
        let order = prepare_simulated_order(
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
        assert!(order.limit_price.is_none());
        assert!(order.stop_price.is_none());
    }

    #[test]
    fn order_entry_converts_display_quantity_to_provider_fixed_point_units() {
        let mut frame = order_book_frame();
        frame.quantity_scale = 3;
        let order = prepare_simulated_order(
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
    fn flatten_feedback_reports_broker_closes_as_requests_and_partial_work_as_errors() {
        use aeris_trading_runtime::FlattenOutcome;
        assert_eq!(
            super::flatten_feedback(
                "Flattened",
                Ok(FlattenOutcome {
                    pending_close_requests: vec!["77".into(), "78".into()],
                    ..FlattenOutcome::default()
                })
            ),
            Ok("Flattened · 2 cTrader close request(s) sent".to_string())
        );
        assert_eq!(
            super::flatten_feedback(
                "Flattened all accounts",
                Ok(FlattenOutcome {
                    incomplete: Some("1 cTrader order(s) could not be cancelled".into()),
                    ..FlattenOutcome::default()
                })
            ),
            Err("Flattened all accounts · 1 cTrader order(s) could not be cancelled".to_string())
        );
        assert_eq!(
            super::flatten_feedback("Reversed", Err("no position".into())),
            Err("no position".to_string())
        );
    }

    #[test]
    fn kill_feedback_reports_locks_fills_unpriced_and_partial_work() {
        use aeris_trading_runtime::EmergencyOutcome;
        assert_eq!(
            super::kill_feedback(
                "Kill switch",
                Ok(EmergencyOutcome {
                    accounts_locked: 1,
                    ..EmergencyOutcome::default()
                })
            ),
            Ok("Kill switch: locked 1 account(s)".to_string())
        );
        let unpriced = EmergencyOutcome {
            accounts_locked: 1,
            unpriced: vec![aeris_trading_runtime::UnpricedPosition {
                account_id: aeris_trading::TradingAccountId::try_new("aeris-sim-1")
                    .expect("account"),
                instrument_id: aeris_instruments::InstrumentId::try_new(
                    "instrument:fixture:CME:ESZ6",
                )
                .expect("instrument"),
                net_quantity: aeris_trading::FixedPoint::try_new(1, 0).expect("net"),
            }],
            ..EmergencyOutcome::default()
        };
        assert_eq!(
            super::kill_feedback("Global kill switch", Ok(unpriced)),
            Ok(
                "Global kill switch: locked 1 account(s) · 1 position(s) lack a verified quote and keep their protection"
                    .to_string()
            )
        );
        assert_eq!(
            super::kill_feedback(
                "Kill switch",
                Ok(EmergencyOutcome {
                    accounts_locked: 1,
                    incomplete: Some("accounts are locked; 1 cTrader order(s) could not be cancelled".into()),
                    ..EmergencyOutcome::default()
                })
            ),
            Err(
                "Kill switch: locked 1 account(s) · accounts are locked; 1 cTrader order(s) could not be cancelled"
                    .to_string()
            )
        );
    }

    #[test]
    fn recovering_book_cannot_fill_a_practice_order() {
        let mut frame = order_book_frame();
        frame.state = aeris_market_data::OrderBookState::Stale;
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
