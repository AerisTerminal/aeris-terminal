//! Owner requests translated to Rithmic order-plant commands, and the bookkeeping that
//! attributes Rithmic's replies to them. Replies to new, modify and cancel commands carry
//! the client order id or the basket id; level moves and exits run one at a time, so their
//! reply belongs to the one outstanding request, and cancel-all replies, which carry no
//! identity, are answered in the order they were sent.
//!
//! Every outstanding request has a response deadline. One that passes it, or that is
//! evicted to keep the bookkeeping bounded, is reported to the owner as unanswered and is
//! never resubmitted; the owner reconciles the account instead.

use super::{
    reconcile::Reconcile,
    session::{Session, SessionEnd, lost},
    translate,
};
use aeris_instruments::InstrumentId;
use aeris_observability::diagnostic;
use aeris_rithmic_protocol_adapter::{
    BracketLevelUpdateRequest, BracketOrderRequest, CancelAllOrdersRequest, CancelOrderRequest,
    ExecutionReplayRequest, ExitPositionRequest, ModifyOrderRequest, NewOrderRequest,
    RetryDisposition, RithmicAccount, RithmicBracketLevel, RithmicOrderPlacement, RithmicOrderType,
    RithmicRequestKind, RithmicSessionError, RithmicTradeRoute, parse_rithmic_instrument_id,
};
use aeris_trading::{
    ClientOrderId,
    venue::{
        BracketSide, OperationFailure, VenueAmendment, VenueOperation, VenueOrder, VenueRequest,
        VenueUpdate,
    },
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Order commands awaiting Rithmic's reply; past this bound the oldest is reported as
/// unanswered.
const MAXIMUM_PENDING: usize = 256;
/// Cancel-all requests awaiting Rithmic's reply; past this bound the oldest is reported as
/// unanswered.
const MAXIMUM_PENDING_CANCEL_ALL: usize = 64;
/// Longest wait for Rithmic's reply to an order command, level move, exit or cancel-all.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// The owner's outbound queue holds at most 64 requests, so more waiting requests mean
/// the busy lane is not draining.
const MAXIMUM_WAITING: usize = 64;
const RECONCILE_TIMEOUT: Duration = Duration::from_mins(2);
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const PLACEMENT: RithmicOrderPlacement = RithmicOrderPlacement::Manual;

/// An order command whose reply names the order it refuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Command {
    Place,
    Modify,
    Cancel,
}

impl Command {
    pub(super) const fn of(kind: RithmicRequestKind) -> Option<Self> {
        match kind {
            RithmicRequestKind::NewOrder | RithmicRequestKind::BracketOrder => Some(Self::Place),
            RithmicRequestKind::ModifyOrder => Some(Self::Modify),
            RithmicRequestKind::CancelOrder => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct PendingCommand {
    command: Command,
    /// The client order id of a place, the basket id of a modify or cancel.
    key: String,
    pub(super) broker_account: String,
    pub(super) client_order_id: ClientOrderId,
    deadline: Instant,
}

/// The one outstanding bracket level move.
pub(super) struct PendingLevel {
    pub(super) broker_account: String,
    pub(super) entry: String,
    pub(super) side: BracketSide,
    pub(super) from_ticks: i32,
    pub(super) to_ticks: i32,
    deadline: Instant,
}

impl PendingLevel {
    pub(super) fn operation(&self) -> VenueOperation {
        VenueOperation::MoveBracketLevel {
            entry_broker_order_id: self.entry.clone(),
            side: self.side,
            current_ticks: self.from_ticks,
            new_ticks: self.to_ticks,
        }
    }
}

/// The one outstanding position exit.
pub(super) struct PendingExit {
    pub(super) broker_account: String,
    pub(super) instrument_id: InstrumentId,
    deadline: Instant,
}

struct PendingCancelAll {
    broker_account: String,
    deadline: Instant,
}

/// An operation to report to the owner as failed for one broker account.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Failed {
    pub(super) broker_account: String,
    pub(super) operation: VenueOperation,
}

impl Failed {
    fn command(command: PendingCommand) -> Self {
        Self {
            broker_account: command.broker_account,
            operation: VenueOperation::OrderCommand {
                client_order_id: command.client_order_id,
            },
        }
    }
}

#[derive(Default)]
pub(super) struct Pending {
    commands: VecDeque<PendingCommand>,
    pub(super) level: Option<PendingLevel>,
    pub(super) exit: Option<PendingExit>,
    cancel_alls: VecDeque<PendingCancelAll>,
}

impl Pending {
    /// Records a sent command, returning the oldest one when the bound evicts it.
    fn sent(&mut self, command: PendingCommand) -> Option<Failed> {
        let evicted = (self.commands.len() >= MAXIMUM_PENDING)
            .then(|| self.commands.pop_front())
            .flatten()
            .map(Failed::command);
        self.commands.push_back(command);
        evicted
    }

    /// Records a sent cancel-all, returning the oldest one when the bound evicts it.
    fn sent_cancel_all(&mut self, broker_account: String, deadline: Instant) -> Option<Failed> {
        let evicted = (self.cancel_alls.len() >= MAXIMUM_PENDING_CANCEL_ALL)
            .then(|| self.cancel_alls.pop_front())
            .flatten()
            .map(|evicted| Failed {
                broker_account: evicted.broker_account,
                operation: VenueOperation::CancelAll,
            });
        self.cancel_alls.push_back(PendingCancelAll {
            broker_account,
            deadline,
        });
        evicted
    }

    /// The account of the oldest outstanding cancel-all, which a cancel-all reply answers.
    pub(super) fn cancel_all_answered(&mut self) -> Option<String> {
        self.cancel_alls
            .pop_front()
            .map(|pending| pending.broker_account)
    }

    /// Removes every request whose response deadline has passed. Each queue holds its
    /// entries in deadline order because every entry gets the same timeout from a
    /// monotonic clock.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<Failed> {
        let mut expired = Vec::new();
        while let Some(command) = self
            .commands
            .pop_front_if(|command| command.deadline <= now)
        {
            expired.push(Failed::command(command));
        }
        while let Some(cancel_all) = self
            .cancel_alls
            .pop_front_if(|cancel_all| cancel_all.deadline <= now)
        {
            expired.push(Failed {
                broker_account: cancel_all.broker_account,
                operation: VenueOperation::CancelAll,
            });
        }
        if let Some(level) = self.level.take_if(|level| level.deadline <= now) {
            expired.push(Failed {
                operation: level.operation(),
                broker_account: level.broker_account,
            });
        }
        if let Some(exit) = self.exit.take_if(|exit| exit.deadline <= now) {
            expired.push(Failed {
                broker_account: exit.broker_account,
                operation: VenueOperation::ExitPosition {
                    instrument_id: exit.instrument_id,
                },
            });
        }
        expired
    }

    /// Removes the command a reply answers. A refusal without the identifying field is
    /// attributed to the oldest command of its kind, because Rithmic answers one session's
    /// commands in order; an acceptance without it identifies nothing.
    pub(super) fn answered(
        &mut self,
        command: Command,
        key: Option<&str>,
        refused: bool,
    ) -> Option<PendingCommand> {
        let index = match key {
            Some(key) => self
                .commands
                .iter()
                .position(|pending| pending.command == command && pending.key == key),
            None if refused => self
                .commands
                .iter()
                .position(|pending| pending.command == command),
            None => None,
        }?;
        self.commands.remove(index)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dispatch {
    Done,
    /// The request's Rithmic lane is busy; retry it later.
    Busy,
}

enum Outcome {
    Sent,
    Busy,
    Refused(String),
}

fn sent(result: Result<(), RithmicSessionError>) -> Result<Outcome, SessionEnd> {
    match result {
        Ok(()) => Ok(Outcome::Sent),
        Err(RithmicSessionError::RequestInFlight) => Ok(Outcome::Busy),
        Err(error) if matches!(error.retry_disposition(), RetryDisposition::Transient) => {
            Err(lost("sending a request")(error))
        }
        Err(error) => Ok(Outcome::Refused(error.to_string())),
    }
}

fn find<'a>(accounts: &'a [RithmicAccount], broker_account: &str) -> Option<&'a RithmicAccount> {
    accounts
        .iter()
        .find(|account| account.account_id.as_str() == broker_account)
}

/// The account's route to `exchange`, preferring Rithmic's default route.
pub(super) fn trade_route<'a>(
    routes: &'a [RithmicTradeRoute],
    account: &RithmicAccount,
    exchange: &str,
) -> Option<&'a RithmicTradeRoute> {
    routes
        .iter()
        .filter(|route| {
            route.exchange == exchange
                && route.fcm_id == account.fcm_id
                && route.ib_id == account.ib_id
        })
        .max_by_key(|route| route.is_default == Some(true))
}

fn contract(instrument_id: &InstrumentId) -> Result<(&str, &str), String> {
    parse_rithmic_instrument_id(instrument_id.as_str())
        .ok_or_else(|| "the instrument is not a Rithmic contract".to_string())
}

const NOT_CONNECTED: &str = "the broker account is not connected";

struct PlaceTerms<'a> {
    exchange: &'a str,
    symbol: &'a str,
    quantity: u32,
    order_type: RithmicOrderType,
    bracket: Option<(Vec<RithmicBracketLevel>, Vec<RithmicBracketLevel>)>,
}

fn place_terms(order: &VenueOrder) -> Result<PlaceTerms<'_>, String> {
    let (exchange, symbol) = contract(&order.instrument_id)?;
    let bracket = match &order.bracket {
        Some(bracket) => Some((
            translate::bracket_levels(&bracket.targets)?,
            translate::bracket_levels(&bracket.stops)?,
        )),
        None => None,
    };
    Ok(PlaceTerms {
        exchange,
        symbol,
        quantity: translate::contracts(order.quantity)?,
        order_type: translate::order_type(order.order_type, order.limit_price, order.stop_price)?,
        bracket,
    })
}

/// The replay window, in whole seconds, from `since` through now. `None` when it does not
/// fit Rithmic's 32-bit seconds.
pub(super) fn replay_window(since_unix_nanos: i64, now_unix_nanos: i64) -> Option<(i32, i32)> {
    let finish = i32::try_from(now_unix_nanos.div_euclid(NANOS_PER_SECOND)).ok()?;
    let start = i32::try_from(since_unix_nanos.div_euclid(NANOS_PER_SECOND)).ok()?;
    Some((start.min(finish), finish))
}

impl Session<'_> {
    pub(super) fn submit(&mut self, request: VenueRequest) -> Result<(), SessionEnd> {
        if self.dispatch(&request)? == Dispatch::Busy {
            if self.waiting.len() >= MAXIMUM_WAITING {
                return Err(SessionEnd::Lost(
                    "venue requests backed up behind a busy Rithmic request".into(),
                ));
            }
            self.waiting.push_back(request);
        }
        Ok(())
    }

    /// Retries each waiting request once, keeping their order.
    pub(super) fn retry_waiting(&mut self) -> Result<(), SessionEnd> {
        for _ in 0..self.waiting.len() {
            let Some(request) = self.waiting.pop_front() else {
                break;
            };
            if self.dispatch(&request)? == Dispatch::Busy {
                self.waiting.push_back(request);
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, request: &VenueRequest) -> Result<Dispatch, SessionEnd> {
        match request {
            VenueRequest::Place(order) => self.place(order),
            VenueRequest::Amend(amendment) => self.amend(amendment),
            VenueRequest::Cancel {
                client_order_id,
                broker_account,
                broker_order_id,
            } => self.cancel(client_order_id, broker_account, broker_order_id),
            VenueRequest::MoveBracketLevel {
                broker_account,
                entry_broker_order_id,
                side,
                current_ticks,
                new_ticks,
            } => self.move_level(PendingLevel {
                broker_account: broker_account.clone(),
                entry: entry_broker_order_id.clone(),
                side: *side,
                from_ticks: *current_ticks,
                to_ticks: *new_ticks,
                deadline: self.command_deadline(),
            }),
            VenueRequest::CancelAll { broker_account } => self.cancel_all(broker_account),
            VenueRequest::ExitPosition {
                broker_account,
                instrument_id,
            } => self.exit_position(broker_account, instrument_id),
            VenueRequest::Reconcile {
                broker_account,
                deals_since_unix_nanos,
            } => self.start_reconcile(broker_account, *deals_since_unix_nanos),
        }
    }

    fn place(&mut self, order: &VenueOrder) -> Result<Dispatch, SessionEnd> {
        let outcome = match (
            place_terms(order),
            find(&self.accounts, &order.broker_account),
        ) {
            (Err(reason), _) => Outcome::Refused(reason),
            (_, None) => Outcome::Refused(NOT_CONNECTED.into()),
            (Ok(terms), Some(account)) => {
                match trade_route(&self.routes, account, terms.exchange) {
                    None => Outcome::Refused(format!(
                        "no Rithmic trade route serves {}",
                        terms.exchange
                    )),
                    Some(route) => {
                        let entry = NewOrderRequest {
                            account: account.key(),
                            user_tag: order.client_order_id.as_str(),
                            symbol: terms.symbol,
                            exchange: terms.exchange,
                            trade_route: &route.trade_route,
                            side: translate::order_side(order.side),
                            quantity: terms.quantity,
                            duration: translate::duration(order.time_in_force),
                            order_type: terms.order_type,
                            placement: PLACEMENT,
                        };
                        sent(match &terms.bracket {
                            None => self.plants.orders.submit_order(entry),
                            Some((targets, stops)) => {
                                self.plants
                                    .orders
                                    .submit_bracket_order(BracketOrderRequest {
                                        entry,
                                        user_type: self.user_type,
                                        targets,
                                        stops,
                                    })
                            }
                        })?
                    }
                }
            }
        };
        self.settle(
            outcome,
            PendingCommand {
                command: Command::Place,
                key: order.client_order_id.as_str().to_string(),
                broker_account: order.broker_account.clone(),
                client_order_id: order.client_order_id.clone(),
                deadline: self.command_deadline(),
            },
        )
    }

    fn amend(&mut self, amendment: &VenueAmendment) -> Result<Dispatch, SessionEnd> {
        let terms = contract(&amendment.instrument_id).and_then(|(exchange, symbol)| {
            Ok((
                exchange,
                symbol,
                translate::contracts(amendment.quantity)?,
                translate::order_type(
                    amendment.order_type,
                    amendment.limit_price,
                    amendment.stop_price,
                )?,
            ))
        });
        let outcome = match (terms, find(&self.accounts, &amendment.broker_account)) {
            (Err(reason), _) => Outcome::Refused(reason),
            (_, None) => Outcome::Refused(NOT_CONNECTED.into()),
            (Ok((exchange, symbol, quantity, order_type)), Some(account)) => {
                sent(self.plants.orders.modify_order(ModifyOrderRequest {
                    account: account.key(),
                    basket_id: &amendment.broker_order_id,
                    symbol,
                    exchange,
                    quantity,
                    order_type,
                    placement: PLACEMENT,
                }))?
            }
        };
        self.settle(
            outcome,
            PendingCommand {
                command: Command::Modify,
                key: amendment.broker_order_id.clone(),
                broker_account: amendment.broker_account.clone(),
                client_order_id: amendment.client_order_id.clone(),
                deadline: self.command_deadline(),
            },
        )
    }

    fn cancel(
        &mut self,
        client_order_id: &ClientOrderId,
        broker_account: &str,
        broker_order_id: &str,
    ) -> Result<Dispatch, SessionEnd> {
        let outcome = match find(&self.accounts, broker_account) {
            None => Outcome::Refused(NOT_CONNECTED.into()),
            Some(account) => sent(self.plants.orders.cancel_order(CancelOrderRequest {
                account: account.key(),
                basket_id: broker_order_id,
                placement: PLACEMENT,
            }))?,
        };
        self.settle(
            outcome,
            PendingCommand {
                command: Command::Cancel,
                key: broker_order_id.to_string(),
                broker_account: broker_account.to_string(),
                client_order_id: client_order_id.clone(),
                deadline: self.command_deadline(),
            },
        )
    }

    /// Records a sent command for its reply, or reports a refused one to the owner.
    fn settle(
        &mut self,
        outcome: Outcome,
        command: PendingCommand,
    ) -> Result<Dispatch, SessionEnd> {
        match outcome {
            Outcome::Sent => {
                if let Some(evicted) = self.pending.sent(command) {
                    self.report_evicted(evicted, MAXIMUM_PENDING)?;
                }
                Ok(Dispatch::Done)
            }
            Outcome::Busy => Ok(Dispatch::Busy),
            Outcome::Refused(reason) => {
                self.emit(
                    &command.broker_account,
                    VenueUpdate::Refused {
                        client_order_id: command.client_order_id,
                        reason,
                    },
                )?;
                Ok(Dispatch::Done)
            }
        }
    }

    fn command_deadline(&self) -> Instant {
        self.context.clock.monotonic() + COMMAND_TIMEOUT
    }

    pub(super) fn report_failed(
        &self,
        failed: Failed,
        failure: OperationFailure,
        reason: String,
    ) -> Result<(), SessionEnd> {
        self.emit(
            &failed.broker_account,
            VenueUpdate::OperationFailed {
                operation: failed.operation,
                failure,
                reason,
            },
        )
    }

    pub(super) fn refuse(
        &self,
        broker_account: &str,
        operation: VenueOperation,
        reason: String,
    ) -> Result<(), SessionEnd> {
        let failed = Failed {
            broker_account: broker_account.to_string(),
            operation,
        };
        self.report_failed(failed, OperationFailure::Refused, reason)
    }

    fn report_evicted(&self, evicted: Failed, bound: usize) -> Result<(), SessionEnd> {
        diagnostic!("Aeris broker venue stopped waiting for a Rithmic reply beyond its bound");
        self.report_failed(
            evicted,
            OperationFailure::Unanswered,
            format!("more than {bound} requests awaited Rithmic's reply"),
        )
    }

    /// Reports every outstanding request Rithmic has not answered within its deadline.
    pub(super) fn expire_pending(&mut self, now: Instant) -> Result<(), SessionEnd> {
        let expired = self.pending.expire(now);
        if !expired.is_empty() {
            diagnostic!(
                "Aeris broker venue: Rithmic did not answer {} requests in time",
                expired.len()
            );
        }
        for failed in expired {
            self.report_failed(
                failed,
                OperationFailure::Unanswered,
                format!(
                    "Rithmic did not answer within {} seconds",
                    COMMAND_TIMEOUT.as_secs()
                ),
            )?;
        }
        Ok(())
    }

    fn move_level(&mut self, level: PendingLevel) -> Result<Dispatch, SessionEnd> {
        let Some(account) = find(&self.accounts, &level.broker_account) else {
            diagnostic!("Aeris broker venue could not move a bracket level: {NOT_CONNECTED}");
            self.refuse(
                &level.broker_account,
                level.operation(),
                NOT_CONNECTED.into(),
            )?;
            return Ok(Dispatch::Done);
        };
        let request = BracketLevelUpdateRequest {
            account: account.key(),
            basket_id: &level.entry,
            current_ticks: level.from_ticks,
            new_ticks: level.to_ticks,
        };
        let result = match level.side {
            BracketSide::Target => self.plants.orders.update_target_level(request),
            BracketSide::Stop => self.plants.orders.update_stop_level(request),
        };
        Ok(match sent(result)? {
            Outcome::Sent => {
                self.pending.level = Some(level);
                Dispatch::Done
            }
            Outcome::Busy => Dispatch::Busy,
            Outcome::Refused(reason) => {
                diagnostic!("Aeris broker venue could not move a bracket level: {reason}");
                self.refuse(&level.broker_account, level.operation(), reason)?;
                Dispatch::Done
            }
        })
    }

    fn cancel_all(&mut self, broker_account: &str) -> Result<Dispatch, SessionEnd> {
        let outcome =
            match find(&self.accounts, broker_account) {
                None => Outcome::Refused(NOT_CONNECTED.into()),
                Some(account) => sent(self.plants.orders.cancel_all_orders(
                    CancelAllOrdersRequest {
                        account: account.key(),
                        user_type: self.user_type,
                        placement: PLACEMENT,
                    },
                ))?,
            };
        Ok(match outcome {
            Outcome::Sent => {
                let deadline = self.command_deadline();
                if let Some(evicted) = self
                    .pending
                    .sent_cancel_all(broker_account.to_string(), deadline)
                {
                    self.report_evicted(evicted, MAXIMUM_PENDING_CANCEL_ALL)?;
                }
                Dispatch::Done
            }
            Outcome::Busy => Dispatch::Busy,
            Outcome::Refused(reason) => {
                diagnostic!("Aeris broker venue could not cancel all orders: {reason}");
                self.refuse(broker_account, VenueOperation::CancelAll, reason)?;
                Dispatch::Done
            }
        })
    }

    fn exit_position(
        &mut self,
        broker_account: &str,
        instrument_id: &InstrumentId,
    ) -> Result<Dispatch, SessionEnd> {
        let outcome = match (
            contract(instrument_id),
            find(&self.accounts, broker_account),
        ) {
            (Err(reason), _) => Outcome::Refused(reason),
            (_, None) => Outcome::Refused(NOT_CONNECTED.into()),
            (Ok((exchange, symbol)), Some(account)) => {
                sent(self.plants.orders.exit_position(ExitPositionRequest {
                    account: account.key(),
                    symbol,
                    exchange,
                    placement: PLACEMENT,
                }))?
            }
        };
        Ok(match outcome {
            Outcome::Sent => {
                self.pending.exit = Some(PendingExit {
                    broker_account: broker_account.to_string(),
                    instrument_id: instrument_id.clone(),
                    deadline: self.command_deadline(),
                });
                Dispatch::Done
            }
            Outcome::Busy => Dispatch::Busy,
            Outcome::Refused(reason) => {
                diagnostic!("Aeris broker venue could not exit a position: {reason}");
                let operation = VenueOperation::ExitPosition {
                    instrument_id: instrument_id.clone(),
                };
                self.refuse(broker_account, operation, reason)?;
                Dispatch::Done
            }
        })
    }

    /// Replays the account's executions since `since`, then requests its order and
    /// position snapshots. One reconcile runs at a time.
    fn start_reconcile(
        &mut self,
        broker_account: &str,
        since: Option<i64>,
    ) -> Result<Dispatch, SessionEnd> {
        if self.reconcile.is_some() {
            return Ok(Dispatch::Busy);
        }
        let Some(account) = find(&self.accounts, broker_account) else {
            self.refuse(
                broker_account,
                VenueOperation::Reconcile,
                NOT_CONNECTED.into(),
            )?;
            return Ok(Dispatch::Done);
        };
        let deadline = self.context.clock.monotonic() + RECONCILE_TIMEOUT;
        let Some(since) = since else {
            self.begin_snapshot(broker_account.to_string(), deadline)?;
            return Ok(Dispatch::Done);
        };
        let Some((start_seconds, finish_seconds)) =
            replay_window(since, self.context.wall_nanos()?)
        else {
            diagnostic!("Aeris broker venue cannot replay executions outside Rithmic's range");
            self.begin_snapshot(broker_account.to_string(), deadline)?;
            return Ok(Dispatch::Done);
        };
        match sent(
            self.plants
                .orders
                .replay_executions(ExecutionReplayRequest {
                    account: account.key(),
                    start_seconds,
                    finish_seconds,
                }),
        )? {
            Outcome::Sent => {
                self.reconcile = Some(Reconcile::replaying(broker_account.to_string(), deadline));
                Ok(Dispatch::Done)
            }
            Outcome::Busy => Ok(Dispatch::Busy),
            Outcome::Refused(reason) => {
                diagnostic!("Aeris broker venue could not replay executions: {reason}");
                self.begin_snapshot(broker_account.to_string(), deadline)?;
                Ok(Dispatch::Done)
            }
        }
    }

    /// Requests the account's working orders and positions; both answers together form
    /// the reconcile's snapshot.
    pub(super) fn begin_snapshot(
        &mut self,
        broker_account: String,
        deadline: Instant,
    ) -> Result<(), SessionEnd> {
        self.reconcile = None;
        let Some(account) = find(&self.accounts, &broker_account) else {
            return self.refuse(
                &broker_account,
                VenueOperation::Reconcile,
                NOT_CONNECTED.into(),
            );
        };
        self.plants
            .orders
            .request_order_snapshot(account.key())
            .map_err(lost("the order snapshot request"))?;
        self.plants
            .pnl
            .request_position_snapshot(account.key())
            .map_err(lost("the position snapshot request"))?;
        self.reconcile = Some(Reconcile::snapshotting(broker_account, deadline));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(exchange: &str, fcm: &str, name: &str, default: Option<bool>) -> RithmicTradeRoute {
        RithmicTradeRoute {
            fcm_id: fcm.into(),
            ib_id: "ib".into(),
            exchange: exchange.into(),
            trade_route: name.into(),
            status: None,
            is_default: default,
        }
    }

    fn account() -> RithmicAccount {
        RithmicAccount {
            fcm_id: "fcm".into(),
            ib_id: "ib".into(),
            account_id: aeris_rithmic_protocol_adapter::RithmicAccountRef::new("account-1"),
            account_name: None,
            currency: Some("USD".into()),
        }
    }

    fn command(command: Command, key: &str, client: &str) -> PendingCommand {
        command_until(command, key, client, Instant::now() + COMMAND_TIMEOUT)
    }

    fn command_until(
        command: Command,
        key: &str,
        client: &str,
        deadline: Instant,
    ) -> PendingCommand {
        PendingCommand {
            command,
            key: key.into(),
            broker_account: "account-1".into(),
            client_order_id: ClientOrderId::try_new(client).expect("client id"),
            deadline,
        }
    }

    fn order_command(account: &str, client: &str) -> Failed {
        Failed {
            broker_account: account.into(),
            operation: VenueOperation::OrderCommand {
                client_order_id: ClientOrderId::try_new(client).expect("client id"),
            },
        }
    }

    #[test]
    fn orders_take_the_accounts_default_route_to_the_exchange() {
        let routes = [
            route("CME", "fcm", "secondary", Some(false)),
            route("CME", "other", "foreign", Some(true)),
            route("CME", "fcm", "primary", Some(true)),
            route("CBOT", "fcm", "grains", Some(true)),
        ];
        let chosen = trade_route(&routes, &account(), "CME").expect("route");
        assert_eq!(chosen.trade_route, "primary");
        assert!(trade_route(&routes, &account(), "NYMEX").is_none());
    }

    #[test]
    fn replies_are_attributed_by_identity_and_refusals_fall_back_to_the_oldest() {
        let mut pending = Pending::default();
        pending.sent(command(Command::Place, "client-1", "client-1"));
        pending.sent(command(Command::Modify, "basket-1", "client-2"));
        pending.sent(command(Command::Modify, "basket-2", "client-3"));
        assert!(pending.answered(Command::Modify, None, false).is_none());
        let refused = pending
            .answered(Command::Modify, Some("basket-2"), true)
            .expect("by basket");
        assert_eq!(refused.client_order_id.as_str(), "client-3");
        let oldest = pending
            .answered(Command::Modify, None, true)
            .expect("oldest modify");
        assert_eq!(oldest.client_order_id.as_str(), "client-2");
        assert!(pending.answered(Command::Cancel, None, true).is_none());
        assert!(
            pending
                .answered(Command::Place, Some("client-1"), false)
                .is_some()
        );
    }

    #[test]
    fn commands_evicted_beyond_the_bound_are_reported_unanswered() {
        let mut pending = Pending::default();
        for index in 0..MAXIMUM_PENDING {
            let client = format!("client-{index}");
            let sent = command(Command::Cancel, &index.to_string(), &client);
            assert_eq!(pending.sent(sent), None);
        }
        let evicted = pending.sent(command(Command::Cancel, "last", "client-last"));
        assert_eq!(evicted, Some(order_command("account-1", "client-0")));
        assert!(pending.answered(Command::Cancel, Some("0"), true).is_none());
        assert!(pending.answered(Command::Cancel, Some("1"), true).is_some());

        let deadline = Instant::now() + COMMAND_TIMEOUT;
        for index in 0..MAXIMUM_PENDING_CANCEL_ALL {
            assert_eq!(
                pending.sent_cancel_all(format!("account-{index}"), deadline),
                None
            );
        }
        assert_eq!(
            pending.sent_cancel_all("account-last".into(), deadline),
            Some(Failed {
                broker_account: "account-0".into(),
                operation: VenueOperation::CancelAll,
            })
        );
        assert_eq!(pending.cancel_all_answered().as_deref(), Some("account-1"));
    }

    #[test]
    fn requests_past_their_deadline_expire_once_and_answered_ones_never_do() {
        let start = Instant::now();
        let early = start + COMMAND_TIMEOUT;
        let late = early + Duration::from_secs(1);
        let mut pending = Pending::default();
        pending.sent(command_until(Command::Place, "client-1", "client-1", early));
        pending.sent(command_until(
            Command::Modify,
            "basket-2",
            "client-2",
            early,
        ));
        pending.sent(command_until(Command::Cancel, "basket-3", "client-3", late));
        assert!(
            pending
                .answered(Command::Modify, Some("basket-2"), false)
                .is_some()
        );
        pending.sent_cancel_all("account-1".into(), early);
        pending.sent_cancel_all("account-2".into(), late);
        pending.level = Some(PendingLevel {
            broker_account: "account-3".into(),
            entry: "entry".into(),
            side: BracketSide::Stop,
            from_ticks: 8,
            to_ticks: 12,
            deadline: early,
        });
        let instrument_id = InstrumentId::try_new("instrument:rithmic:CME:ESZ5").expect("id");
        pending.exit = Some(PendingExit {
            broker_account: "account-4".into(),
            instrument_id: instrument_id.clone(),
            deadline: late,
        });

        assert_eq!(pending.expire(start), Vec::new());
        assert_eq!(
            pending.expire(early),
            vec![
                order_command("account-1", "client-1"),
                Failed {
                    broker_account: "account-1".into(),
                    operation: VenueOperation::CancelAll,
                },
                Failed {
                    broker_account: "account-3".into(),
                    operation: VenueOperation::MoveBracketLevel {
                        entry_broker_order_id: "entry".into(),
                        side: BracketSide::Stop,
                        current_ticks: 8,
                        new_ticks: 12,
                    },
                },
            ]
        );
        assert_eq!(pending.expire(early), Vec::new(), "each expires once");
        assert_eq!(
            pending.expire(late),
            vec![
                order_command("account-1", "client-3"),
                Failed {
                    broker_account: "account-2".into(),
                    operation: VenueOperation::CancelAll,
                },
                Failed {
                    broker_account: "account-4".into(),
                    operation: VenueOperation::ExitPosition { instrument_id },
                },
            ]
        );
        assert!(pending.level.is_none() && pending.exit.is_none());
        assert_eq!(pending.cancel_all_answered(), None);
    }

    #[test]
    fn replay_windows_cover_whole_seconds_through_now() {
        assert_eq!(
            replay_window(1_700_000_000_900_000_000, 1_700_000_100_000_000_000),
            Some((1_700_000_000, 1_700_000_100))
        );
        assert_eq!(
            replay_window(1_700_000_200_000_000_000, 1_700_000_100_000_000_000),
            Some((1_700_000_100, 1_700_000_100))
        );
        assert_eq!(replay_window(1, i64::MAX), None);
    }
}
