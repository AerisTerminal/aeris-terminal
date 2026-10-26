//! Desktop command access to the single in-process trading owner.

use aeris_instruments::InstrumentId;
use aeris_trading_runtime::clock::{Clock, SystemClock};
use aeris_trading_runtime::{
    CreatePracticeAccount, ModifyOrder, PlaceBracket, PlaceOrder, TradeCopierConfig, TradingService,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

/// At most 64 desktop commands may await a UI result. Holding completed results in the
/// same map also bounds a suspended window without silently dropping command outcomes.
pub const MAXIMUM_IN_FLIGHT_TRADING_REQUESTS: usize = 64;
const REQUEST_CAPACITY_NOTICE: &str = "Too many trading commands are waiting for a result";
/// Allows a burst of global commands and venue notices between UI wakes without loss.
pub const MAXIMUM_UNSCOPED_NOTICES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

/// The pane owns this marker. A completion whose marker has expired belongs to the
/// notice center, never to a replacement pane with a reused layout identifier.
#[derive(Clone, Default)]
pub struct TradingPaneToken(Arc<()>);

impl TradingPaneToken {
    #[must_use]
    pub fn owns(&self, origin: &Weak<()>) -> bool {
        Weak::ptr_eq(&Arc::downgrade(&self.0), origin)
    }
}

struct PendingRequest {
    origin: Weak<()>,
    result: Option<TradingCommandFeedback>,
}

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
    unscoped_notices: VecDeque<TradingCommandFeedback>,
    dropped_unscoped_notices: usize,
    next_request: u64,
    in_flight: BTreeMap<RequestId, PendingRequest>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The sequence of the newest broker venue notice already shown.
    venue_notice_seen: u64,
}

/// The status of commands issued by one desktop instance, shared with its panes.
#[derive(Clone)]
pub struct TradingFeedback {
    state: Arc<Mutex<TradingFeedbackState>>,
    clock: Arc<dyn Clock>,
}

impl Default for TradingFeedback {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemClock::default()))
    }
}

impl TradingFeedback {
    pub fn set_request_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wake = Some(wake);
    }

    #[must_use]
    pub fn pane(&self) -> TradingPaneToken {
        TradingPaneToken::default()
    }

    /// Reserve before calling the runtime. Overflow reports exactly one error and the
    /// caller must not execute the command when this returns None.
    #[must_use]
    pub fn begin_request(&self, pane: &TradingPaneToken) -> Option<RequestId> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.in_flight.len() >= MAXIMUM_IN_FLIGHT_TRADING_REQUESTS {
            drop(state);
            self.record(Err(REQUEST_CAPACITY_NOTICE.to_string()));
            return None;
        }
        state.next_request = state.next_request.checked_add(1)?;
        let id = RequestId(state.next_request);
        state.in_flight.insert(
            id,
            PendingRequest {
                origin: Arc::downgrade(&pane.0),
                result: None,
            },
        );
        Some(id)
    }

    pub fn finish_request(&self, id: RequestId, result: Result<String, String>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = state.revision.saturating_add(1).max(1);
        let revision = state.revision;
        if let Some(request) = state.in_flight.get_mut(&id) {
            request.result = Some(match result {
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
        let wake = state.wake.clone();
        drop(state);
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Local pane refusal follows the same route as an owner result.
    pub fn record_for_pane(&self, pane: &TradingPaneToken, result: Result<String, String>) {
        if let Some(id) = self.begin_request(pane) {
            self.finish_request(id, result);
        }
    }

    /// Reports acceptance to chart intent resolution without sending a global notice.
    #[must_use]
    pub fn finish_pane_result(&self, id: RequestId, result: Result<String, String>) -> bool {
        let accepted = result.is_ok();
        self.finish_request(id, result);
        accepted
    }

    #[must_use]
    pub fn has_request_results(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.unscoped_notices.is_empty()
            || state.dropped_unscoped_notices != 0
            || state
                .in_flight
                .values()
                .any(|request| request.result.is_some())
    }

    #[must_use]
    pub fn take_unscoped_notice(&self) -> Option<TradingCommandFeedback> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.dropped_unscoped_notices != 0 {
            let count = std::mem::take(&mut state.dropped_unscoped_notices);
            return Some(TradingCommandFeedback {
                revision: state.revision,
                message: format!("{count} notices dropped"),
                is_error: true,
            });
        }
        state.unscoped_notices.pop_front()
    }

    /// Completed results are consumed once by the UI; unfinished reservations retain
    /// their capacity. A dead pane's weak marker tells the caller to use the notice center.
    #[must_use]
    pub fn take_request_results(
        &self,
    ) -> Vec<(RequestId, Option<Weak<()>>, TradingCommandFeedback)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let complete: Vec<_> = state
            .in_flight
            .iter()
            .filter_map(|(id, request)| request.result.as_ref().map(|_| *id))
            .collect();
        complete
            .into_iter()
            .filter_map(|id| {
                let request = state.in_flight.remove(&id)?;
                Some((
                    id,
                    request.origin.upgrade().map(|_| request.origin),
                    request.result?,
                ))
            })
            .collect()
    }

    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TradingFeedbackState::default())),
            clock,
        }
    }

    #[must_use]
    pub fn now(&self) -> i64 {
        self.clock.wall_unix_nanos()
    }

    pub fn record(&self, result: Result<String, String>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = state.revision.saturating_add(1).max(1);
        let revision = state.revision;
        state.latest = Some(match result {
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
        if state.unscoped_notices.len() == MAXIMUM_UNSCOPED_NOTICES {
            state.unscoped_notices.pop_front();
            state.dropped_unscoped_notices = state.dropped_unscoped_notices.saturating_add(1);
        }
        if let Some(notice) = state.latest.clone() {
            state.unscoped_notices.push_back(notice);
        }
        let wake = state.wake.clone();
        drop(state);
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Shows the trading owner's venue notices this instance has not shown yet as one error
    /// line, newest last. A notice shows once; older ones are still in the diagnostics log.
    pub fn record_venue_notices(&self, notices: &[aeris_trading_runtime::VenueNotice]) {
        let unseen = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let unseen = notices
                .iter()
                .filter(|notice| notice.sequence > state.venue_notice_seen)
                .map(|notice| notice.detail.as_str())
                .collect::<Vec<_>>()
                .join(" · ");
            if let Some(newest) = notices.iter().map(|notice| notice.sequence).max() {
                state.venue_notice_seen = state.venue_notice_seen.max(newest);
            }
            unseen
        };
        if !unseen.is_empty() {
            self.record(Err(unseen));
        }
    }

    #[must_use]
    pub fn latest(&self) -> Option<TradingCommandFeedback> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest
            .clone()
    }
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
    if !outcome.pending_exits.is_empty() {
        parts.push(format!(
            "{} broker exit request(s) sent",
            outcome.pending_exits.len()
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
    if !outcome.pending_exits.is_empty() {
        parts.push(format!(
            "{} broker exit request(s) sent",
            outcome.pending_exits.len()
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

pub fn record_outcome<T>(
    feedback: &TradingFeedback,
    result: Result<T, String>,
    success: &str,
) -> bool {
    record_result(feedback, result.map(|_| success.to_string()))
}

/// Shows a message in the trading status row for something the desktop decided rather than a
/// command result, such as a shortcut refused while keyboard trading is off.
pub fn record_notice(feedback: &TradingFeedback, notice: Result<String, String>) {
    feedback.record(notice);
}

/// Records feedback whose message the command already composed; returns whether it
/// succeeded.
#[must_use]
pub fn record_result(feedback: &TradingFeedback, result: Result<String, String>) -> bool {
    let accepted = result.is_ok();
    feedback.record(result);
    accepted
}

/// Creates a durable user-named practice account off the UI thread.
pub fn create_practice_account(
    service: &TradingService,
    feedback: &TradingFeedback,
    name: String,
    equity: &str,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let starting_equity = match parse_usd_equity(equity) {
        Ok(value) => value,
        Err(error) => {
            feedback.record(Err(error));
            return;
        }
    };
    cx.background_executor()
        .spawn(async move {
            feedback.record(
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
pub fn delete_practice_account(
    service: &TradingService,
    feedback: &TradingFeedback,
    account_key: String,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let account_id = match aeris_trading::TradingAccountId::try_new(account_key) {
        Ok(account_id) => account_id,
        Err(error) => {
            feedback.record(Err(error.to_string()));
            return;
        }
    };
    cx.background_executor()
        .spawn(async move {
            feedback.record(
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

/// Dispatches the selected simulated order-entry instruction off the UI thread.
pub fn dispatch_simulated_order(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    selection: SimulatedOrderSelection,
    cx: &mut gpui::App,
) {
    if order_type_requires_price(selection.order_type) {
        feedback.record_for_pane(pane, Err(STOP_PRICE_REQUIRED.to_string()));
        return;
    }
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(command) = prepare_simulated_order(
        feedback.now(),
        frame,
        side,
        selection.account_key,
        selection.quantity,
        selection.order_type,
        selection.time_in_force,
    ) else {
        feedback.record_for_pane(
            pane,
            Err("Order requires a current bid, ask, and valid instrument".to_string()),
        );
        return;
    };
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let result = service
                .place_order_from_market(command, true)
                .map(|order| order_outcome(&order, OrderOutcome::Placed));
            feedback.finish_request(request_id, result);
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
    service: &TradingService,
    feedback: &TradingFeedback,
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
    selection: SimulatedOrderSelection,
    pane: &TradingPaneToken,
    cx: &mut gpui::App,
) {
    if order_type_requires_price(selection.order_type) {
        feedback.record_for_pane(pane, Err(STOP_PRICE_REQUIRED.to_string()));
        return;
    }
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(entry) = prepare_simulated_order(
        feedback.now(),
        frame,
        side,
        selection.account_key,
        selection.quantity,
        selection.order_type,
        selection.time_in_force,
    ) else {
        feedback.record_for_pane(
            pane,
            Err("Order requires a current bid, ask, and valid instrument".to_string()),
        );
        return;
    };
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let result = if let Some(template_id) = selection.template_id {
                service
                    .place_bracket_from_market(PlaceBracket { entry, template_id })
                    .map(|placement| bracket_outcome(&placement))
            } else {
                service
                    .place_order_from_market(entry, true)
                    .map(|order| order_outcome(&order, OrderOutcome::Placed))
            };
            feedback.finish_request(request_id, result);
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

/// What a command did to one order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderOutcome {
    Placed,
    ProtectionPlaced,
    Modified,
    Cancelled,
}

/// Feedback for one accepted order command, worded for the venue holding the order: a
/// practice order changes at once, while a broker order changes only when the broker
/// reports it.
#[must_use]
pub fn order_outcome(order: &aeris_trading::Order, outcome: OrderOutcome) -> String {
    let id = order.client_order_id.as_str();
    if is_broker_order(order) {
        match outcome {
            OrderOutcome::Placed => format!("Order {id} sent to the broker"),
            OrderOutcome::ProtectionPlaced => format!("Protective order {id} sent to the broker"),
            OrderOutcome::Modified => format!("Change to order {id} sent to the broker"),
            OrderOutcome::Cancelled => format!("Cancel of order {id} sent to the broker"),
        }
    } else {
        match outcome {
            OrderOutcome::Placed => format!("Practice order {id} accepted"),
            OrderOutcome::ProtectionPlaced => format!("Practice protective order {id} placed"),
            OrderOutcome::Modified => format!("Practice order {id} modified"),
            OrderOutcome::Cancelled => format!("Practice order {id} cancelled"),
        }
    }
}

fn is_broker_order(order: &aeris_trading::Order) -> bool {
    order.provenance.venue_id == aeris_trading::venue::BROKER_VENUE_ID
}

/// Feedback for a cancel-all: practice orders are cancelled at once, broker orders are
/// cancel requests until the broker confirms them.
fn cancel_all_outcome(orders: &[aeris_trading::Order]) -> String {
    let broker = orders.iter().filter(|order| is_broker_order(order)).count();
    let practice = orders.len() - broker;
    match (practice, broker) {
        (0, 0) => "No working orders to cancel".to_string(),
        (practice, 0) => format!("Cancelled {practice} practice order(s)"),
        (0, broker) => format!("Cancel sent to the broker for {broker} order(s)"),
        (practice, broker) => format!(
            "Cancelled {practice} practice order(s) · cancel sent to the broker for {broker} order(s)"
        ),
    }
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
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    frame: &aeris_market_data::OrderBookFrame,
    order: SimulatedPricedOrder,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(mut command) = prepare_simulated_order(
        feedback.now(),
        frame,
        order.side,
        order.account_key,
        order.quantity,
        order.order_type,
        order.time_in_force,
    ) else {
        feedback.record_for_pane(
            pane,
            Err("Order requires a current bid, ask, and valid instrument".to_string()),
        );
        return;
    };
    let Ok(price) = aeris_trading::FixedPoint::try_new(order.price_units, frame.price_scale) else {
        feedback.record_for_pane(pane, Err("Order price is invalid".to_string()));
        return;
    };
    (command.limit_price, command.stop_price) = price_fields(order.order_type, price);
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let result = service
                .place_order_from_market(command, false)
                .map(|order| order_outcome(&order, OrderOutcome::Placed));
            feedback.finish_request(request_id, result);
        })
        .detach();
}

/// A stop and target the order ticket attaches to its entry, in ticks from the entry price.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TicketBracket {
    pub stop_ticks: u32,
    pub target_ticks: u32,
}

impl TicketBracket {
    fn template(self, entry: &PlaceOrder) -> aeris_trading_runtime::BracketStrategyTemplate {
        aeris_trading_runtime::BracketStrategyTemplate {
            template_id: format!("ticket-bracket-{}", entry.client_order_id.as_str()),
            revision: 1,
            name: "Order ticket bracket".to_string(),
            stop_offset_ticks: self.stop_ticks,
            targets: vec![aeris_trading_runtime::BracketTarget {
                offset_ticks: self.target_ticks,
                quantity_percent: 100,
            }],
            trailing_stop: None,
            break_even: None,
            enabled: true,
        }
    }
}

/// One order-ticket instruction. A market order carries no price; a resting order carries
/// its exact price on the instrument's grid.
pub struct TicketOrder {
    pub side: aeris_trading::OrderSide,
    pub order_type: aeris_trading::OrderType,
    pub account_key: Option<String>,
    pub quantity: u64,
    pub price_units: Option<i64>,
    pub bracket: Option<TicketBracket>,
}

/// Dispatches an order-ticket entry, with its bracket when one is set, off the UI thread.
pub fn dispatch_ticket_order(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    frame: &aeris_market_data::OrderBookFrame,
    order: TicketOrder,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(mut entry) = prepare_simulated_order(
        feedback.now(),
        frame,
        order.side,
        order.account_key,
        order.quantity,
        order.order_type,
        aeris_trading::TimeInForce::Day,
    ) else {
        feedback.record_for_pane(
            pane,
            Err("Order requires a current bid, ask, and valid instrument".to_string()),
        );
        return;
    };
    if let Some(price_units) = order.price_units {
        let Ok(price) = aeris_trading::FixedPoint::try_new(price_units, frame.price_scale) else {
            feedback.record_for_pane(pane, Err("Order price is invalid".to_string()));
            return;
        };
        (entry.limit_price, entry.stop_price) = price_fields(order.order_type, price);
    }
    let at_touch = order.price_units.is_none();
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let result = match order.bracket {
                Some(bracket) => {
                    let template = bracket.template(&entry);
                    service
                        .place_inline_bracket_from_market(
                            aeris_trading_runtime::PlaceInlineBracket { entry, template },
                        )
                        .map(|placement| bracket_outcome(&placement))
                }
                None => service
                    .place_order_from_market(entry, at_touch)
                    .map(|order| order_outcome(&order, OrderOutcome::Placed)),
            };
            feedback.finish_request(request_id, result);
        })
        .detach();
}

/// Feedback for an accepted bracket, worded for the venue that holds its stop and target.
fn bracket_outcome(placement: &aeris_trading_runtime::PlacementOutcome) -> String {
    match placement {
        aeris_trading_runtime::PlacementOutcome::Managed(bracket) => format!(
            "Practice order {} accepted with its stop and target",
            bracket.entry_client_order_id.as_str()
        ),
        aeris_trading_runtime::PlacementOutcome::BrokerProtected(order) => format!(
            "Order {} sent to the broker with its stop and target",
            order.client_order_id.as_str()
        ),
    }
}

/// Cancels working orders for the selected simulated account off the UI thread.
pub fn cancel_simulated_account(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let account =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok());
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                service
                    .cancel_all(account)
                    .map(|orders| cancel_all_outcome(&orders)),
            );
        })
        .detach();
}

/// Requests the exit of one broker account's whole position on an instrument off the UI
/// thread. The position stays open until the broker reports the exit fills.
pub fn close_broker_position(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    account_id: aeris_trading::TradingAccountId,
    instrument_id: InstrumentId,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                flatten_feedback(
                    "Closing position",
                    service.flatten(aeris_trading_runtime::FlattenScope {
                        account: Some(account_id),
                        instrument: Some(instrument_id),
                    }),
                ),
            );
        })
        .detach();
}

/// Moves one working simulated limit or stop order to an exact ladder price off the UI thread.
pub struct SimulatedOrderModification {
    pub client_order_id: aeris_trading::ClientOrderId,
    pub expected_revision: u32,
    pub order_type: aeris_trading::OrderType,
    pub time_in_force: aeris_trading::TimeInForce,
    pub price: aeris_trading::FixedPoint,
}

pub fn modify_simulated_order_at_price(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    modification: SimulatedOrderModification,
    frame: &aeris_market_data::OrderBookFrame,
    cx: &mut gpui::App,
) {
    let SimulatedOrderModification {
        client_order_id,
        expected_revision,
        order_type,
        time_in_force,
        price,
    } = modification;
    if order_type == aeris_trading::OrderType::Market {
        feedback.record_for_pane(
            pane,
            Err("Market orders have no price to modify".to_string()),
        );
        return;
    }
    let service = service.clone();
    let feedback = feedback.clone();
    let modified_unix_nanos = feedback.now();
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
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                service
                    .modify_order_from_market_checked(
                        aeris_trading_runtime::RevisionedModifyOrder {
                            order: command,
                            expected_revision: Some(expected_revision),
                        },
                    )
                    .map(|order| order_outcome(&order, OrderOutcome::Modified))
                    .map_err(|error| error.to_string()),
            );
        })
        .detach();
}

/// Cancels working orders for every simulated account off the UI thread.
pub fn cancel_simulated_accounts(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                service
                    .cancel_all(None)
                    .map(|orders| cancel_all_outcome(&orders)),
            );
        })
        .detach();
}

/// Locks the selected account, cancels its non-protective orders and flattens every priced position, off the UI thread.
pub fn kill_simulated_account(
    service: &TradingService,
    feedback: &TradingFeedback,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let account =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok());
    let locked_at = feedback.now();
    cx.background_executor()
        .spawn(async move {
            feedback.record(kill_feedback(
                "Kill switch",
                service.kill_switch(account, "manual kill switch".to_string(), locked_at),
            ));
        })
        .detach();
}

/// Clears the durable manual risk lock for one selected simulated account.
pub fn unlock_simulated_account(
    service: &TradingService,
    feedback: &TradingFeedback,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(account) =
        account_key.and_then(|value| aeris_trading::TradingAccountId::try_new(value).ok())
    else {
        feedback.record(Err("Select an account to unlock".to_string()));
        return;
    };
    cx.background_executor()
        .spawn(async move {
            record_outcome(
                &feedback,
                service.unlock_account(account),
                "Account unlocked",
            );
        })
        .detach();
}

/// Locks every tradable account, cancels non-protective orders and flattens every priced position, off the UI thread.
pub fn kill_simulated_accounts(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let locked_at = feedback.now();
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                kill_feedback(
                    "Global kill switch",
                    service.kill_switch(None, "manual global kill switch".to_string(), locked_at),
                ),
            );
        })
        .detach();
}

/// Replaces one runtime-owned trade-copier configuration off the UI thread.
pub fn register_trade_copier(
    service: &TradingService,
    feedback: &TradingFeedback,
    config: TradeCopierConfig,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    cx.background_executor()
        .spawn(async move {
            record_outcome(
                &feedback,
                service.register_trade_copier(config),
                "Practice trade copier updated",
            );
        })
        .detach();
}

/// Prepares a validated local market order and its optional BBO observation.
#[must_use]
pub fn prepare_simulated_market_order(
    submitted_unix_nanos: i64,
    frame: &aeris_market_data::OrderBookFrame,
    side: aeris_trading::OrderSide,
) -> Option<PlaceOrder> {
    prepare_simulated_order(
        submitted_unix_nanos,
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
    submitted_unix_nanos: i64,
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

/// Flattens every instrument of the selected account using the trading owner's quotes.
pub fn flatten_simulated_account(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let Some(scope) = account_flatten_scope(account_key) else {
        feedback.record_for_pane(pane, Err("Flatten requires a selected account".to_string()));
        return;
    };
    let service = service.clone();
    let feedback = feedback.clone();
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                flatten_feedback("Flattened", service.flatten(scope)),
            );
        })
        .detach();
}

/// Flattens only the selected instrument of a simulated account.
pub fn flatten_simulated_account_for(
    service: &TradingService,
    feedback: &TradingFeedback,
    pane: &TradingPaneToken,
    frame: &aeris_market_data::OrderBookFrame,
    account_key: Option<String>,
    cx: &mut gpui::App,
) {
    let service = service.clone();
    let feedback = feedback.clone();
    let Some((account_id, instrument)) = prepare_flatten_for(frame, account_key) else {
        feedback.record_for_pane(
            pane,
            Err("Flatten requires a selected account and current bid and ask".to_string()),
        );
        return;
    };
    let Some(request_id) = feedback.begin_request(pane) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            feedback.finish_request(
                request_id,
                flatten_feedback(
                    "Flattened",
                    service.flatten(aeris_trading_runtime::FlattenScope {
                        account: Some(account_id),
                        instrument: Some(instrument),
                    }),
                ),
            );
        })
        .detach();
}

fn account_flatten_scope(
    account_key: Option<String>,
) -> Option<aeris_trading_runtime::FlattenScope> {
    Some(aeris_trading_runtime::FlattenScope {
        account: Some(aeris_trading::TradingAccountId::try_new(account_key?).ok()?),
        instrument: None,
    })
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

#[cfg(test)]
mod tests {
    use super::{
        MAXIMUM_IN_FLIGHT_TRADING_REQUESTS, TradingFeedback, account_flatten_scope,
        parse_usd_equity, prepare_simulated_order,
    };

    #[test]
    fn pane_request_overflow_rejects_without_calling_the_owner() {
        let feedback = TradingFeedback::default();
        let pane = feedback.pane();
        let mut owner_calls = 0;
        let mut pending = Vec::new();
        for _ in 0..MAXIMUM_IN_FLIGHT_TRADING_REQUESTS {
            let request = feedback.begin_request(&pane).expect("within bound");
            owner_calls += 1;
            pending.push(request);
        }
        assert!(feedback.begin_request(&pane).is_none());
        assert_eq!(owner_calls, MAXIMUM_IN_FLIGHT_TRADING_REQUESTS);
        assert!(feedback.take_request_results().is_empty());
        let overflow = feedback.latest().expect("one overflow notice");
        assert!(overflow.is_error);
        assert_eq!(overflow.message, super::REQUEST_CAPACITY_NOTICE);
        assert_eq!(feedback.take_unscoped_notice(), Some(overflow));
        assert!(
            feedback.take_unscoped_notice().is_none(),
            "exactly one overflow notice"
        );
        feedback.finish_request(pending.remove(0), Ok("accepted".into()));
        let results = feedback.take_request_results();
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_some());
        assert_eq!(results[0].2.message, "accepted");
        assert!(feedback.begin_request(&pane).is_some());
    }

    #[test]
    fn pane_results_follow_the_origin_and_dead_panes_reach_the_center() {
        let feedback = TradingFeedback::default();
        let first = feedback.pane();
        let unrelated = feedback.pane();
        let request = feedback.begin_request(&first).expect("reservation");
        feedback.finish_request(request, Err("rejected".into()));
        let results = feedback.take_request_results();
        assert_eq!(results.len(), 1);
        assert!(first.owns(results[0].1.as_ref().expect("origin")));
        assert!(!unrelated.owns(results[0].1.as_ref().expect("origin")));
        assert_eq!(results[0].2.message, "rejected");
        assert!(feedback.take_request_results().is_empty());
        let retired = feedback.pane();
        let request = feedback.begin_request(&retired).expect("reservation");
        drop(retired);
        feedback.finish_request(request, Ok("completed".into()));
        let results = feedback.take_request_results();
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_none(), "dead pane routes to notice center");
    }

    #[test]
    fn global_notices_are_fifo_and_count_overflow_once() {
        let feedback = TradingFeedback::default();
        for index in 0..3 {
            feedback.record(Ok(format!("global {index}")));
        }
        for index in 0..3 {
            assert_eq!(
                feedback
                    .take_unscoped_notice()
                    .expect("queued notice")
                    .message,
                format!("global {index}")
            );
        }
        assert!(feedback.take_unscoped_notice().is_none());
        for index in 0..super::MAXIMUM_UNSCOPED_NOTICES + 2 {
            feedback.record(Ok(format!("burst {index}")));
        }
        assert_eq!(
            feedback
                .take_unscoped_notice()
                .expect("one loss notice")
                .message,
            "2 notices dropped"
        );
        for index in 2..super::MAXIMUM_UNSCOPED_NOTICES + 2 {
            assert_eq!(
                feedback
                    .take_unscoped_notice()
                    .expect("retained notice")
                    .message,
                format!("burst {index}")
            );
        }
        assert!(feedback.take_unscoped_notice().is_none());
    }

    #[test]
    fn venue_notices_show_once_on_the_feedback_line() {
        let feedback = TradingFeedback::default();
        let notice = |sequence: u64, detail: &str| aeris_trading_runtime::VenueNotice {
            sequence,
            account_id: aeris_trading::TradingAccountId::try_new("broker-demo-1").expect("id"),
            observed_unix_nanos: 1,
            detail: detail.to_string(),
        };
        feedback.record_venue_notices(&[notice(1, "exit refused"), notice(2, "cancel unanswered")]);
        let shown = feedback.latest().expect("notice shown");
        assert!(shown.is_error);
        assert_eq!(shown.message, "exit refused · cancel unanswered");
        feedback.record_venue_notices(&[notice(1, "exit refused"), notice(2, "cancel unanswered")]);
        assert_eq!(
            feedback.latest().expect("unchanged").revision,
            shown.revision
        );
        feedback
            .record_venue_notices(&[notice(2, "cancel unanswered"), notice(3, "reverse dropped")]);
        assert_eq!(feedback.latest().expect("newer").message, "reverse dropped");
    }

    #[test]
    fn account_flatten_command_targets_all_instruments_but_only_the_selected_account() {
        let account = aeris_trading::TradingAccountId::try_new("aeris-sim-2").expect("account");
        assert_eq!(
            account_flatten_scope(Some(account.as_str().to_string())),
            Some(aeris_trading_runtime::FlattenScope {
                account: Some(account),
                instrument: None,
            })
        );
        assert_eq!(account_flatten_scope(None), None);
        assert_eq!(account_flatten_scope(Some(String::new())), None);
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
            order_queues: None,
        }
    }

    #[test]
    fn order_entry_preserves_selected_account_and_execution_instructions() {
        let account_id =
            aeris_trading::TradingAccountId::try_new("aeris-sim-2").expect("account id");
        let order = prepare_simulated_order(
            1_000_000_000,
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
        assert_eq!(order.submitted_unix_nanos, 1_000_000_000);
        assert_eq!(order.provenance.observed_unix_nanos, 1_000_000_000);
    }

    #[test]
    fn order_entry_converts_display_quantity_to_provider_fixed_point_units() {
        let mut frame = order_book_frame();
        frame.quantity_scale = 3;
        let order = prepare_simulated_order(
            1_000_000_000,
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
    fn flatten_feedback_reports_broker_exits_as_requests_and_partial_work_as_errors() {
        use aeris_trading_runtime::{FlattenOutcome, PendingExit};
        let exit = |instrument: &str| PendingExit {
            account_id: aeris_trading::TradingAccountId::try_new("broker-demo-1").expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new(instrument)
                .expect("instrument"),
            net_quantity: aeris_trading::FixedPoint::try_new(2, 0).expect("quantity"),
        };
        assert_eq!(
            super::flatten_feedback(
                "Flattened",
                Ok(FlattenOutcome {
                    pending_exits: vec![
                        exit("instrument:fixture:CME:ESZ6"),
                        exit("instrument:fixture:CME:NQZ6"),
                    ],
                    ..FlattenOutcome::default()
                })
            ),
            Ok("Flattened · 2 broker exit request(s) sent".to_string())
        );
        assert_eq!(
            super::flatten_feedback(
                "Flattened all accounts",
                Ok(FlattenOutcome {
                    incomplete: Some("1 broker order(s) could not be cancelled".into()),
                    ..FlattenOutcome::default()
                })
            ),
            Err("Flattened all accounts · 1 broker order(s) could not be cancelled".to_string())
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
                    incomplete: Some("accounts are locked; 1 broker order(s) could not be cancelled".into()),
                    ..EmergencyOutcome::default()
                })
            ),
            Err(
                "Kill switch: locked 1 account(s) · accounts are locked; 1 broker order(s) could not be cancelled"
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
                1_000_000_000,
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

    fn working_order(venue_id: &str, client_order_id: &str) -> aeris_trading::Order {
        let quantity = aeris_trading::FixedPoint::try_new(1, 0).expect("quantity");
        aeris_trading::Order {
            id: aeris_trading::OrderId::try_new(format!("order-{client_order_id}")).expect("id"),
            client_order_id: aeris_trading::ClientOrderId::try_new(client_order_id)
                .expect("client order id"),
            account_id: aeris_trading::TradingAccountId::try_new("account").expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("instrument:fixture:CME:ESZ6")
                .expect("instrument"),
            side: aeris_trading::OrderSide::Buy,
            order_type: aeris_trading::OrderType::Market,
            time_in_force: aeris_trading::TimeInForce::Day,
            quantity,
            filled_quantity: aeris_trading::FixedPoint::try_new(0, 0).expect("filled"),
            limit_price: None,
            stop_price: None,
            status: aeris_trading::OrderStatus::Working,
            submitted_unix_nanos: 1,
            revision: 1,
            accepted_unix_nanos: 1,
            updated_unix_nanos: 1,
            eligible_after_unix_nanos: 1,
            triggered_unix_nanos: None,
            expires_at_unix_nanos: None,
            terminal_unix_nanos: None,
            provenance: aeris_trading::TradingProvenance {
                venue_id: venue_id.to_string(),
                provider_id: "test-provider".to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: 1,
            },
        }
    }

    #[test]
    fn order_feedback_names_the_venue_that_holds_the_order() {
        use super::{OrderOutcome, cancel_all_outcome, order_outcome};
        let broker = working_order(aeris_trading::venue::BROKER_VENUE_ID, "b1");
        let practice = working_order("aeris-sim", "p1");
        assert_eq!(
            order_outcome(&broker, OrderOutcome::Placed),
            "Order b1 sent to the broker"
        );
        assert_eq!(
            order_outcome(&broker, OrderOutcome::Cancelled),
            "Cancel of order b1 sent to the broker"
        );
        assert_eq!(
            order_outcome(&practice, OrderOutcome::Placed),
            "Practice order p1 accepted"
        );
        assert_eq!(
            order_outcome(&practice, OrderOutcome::Modified),
            "Practice order p1 modified"
        );
        assert_eq!(cancel_all_outcome(&[]), "No working orders to cancel");
        assert_eq!(
            cancel_all_outcome(std::slice::from_ref(&broker)),
            "Cancel sent to the broker for 1 order(s)"
        );
        assert_eq!(
            cancel_all_outcome(&[practice, broker]),
            "Cancelled 1 practice order(s) · cancel sent to the broker for 1 order(s)"
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
