//! Rithmic's order-plant and P&L-plant reports translated to venue updates. Snapshot rows
//! belong to the running reconcile; live reports reach the owner as they arrive.

use super::{
    reconcile::Reconcile,
    requests::Command,
    session::{MAXIMUM_ROUTES, Session, SessionEnd},
    translate,
};
use aeris_observability::diagnostic;
use aeris_rithmic_protocol_adapter::{
    DecodedOrderMessage, DecodedPnlMessage, RithmicAccountPnl, RithmicExchangeOrderNotification,
    RithmicInstrumentPnl, RithmicOrderNotification, RithmicRequestKind, RithmicRequestOutcome,
    RithmicTradeRoute,
};
use aeris_trading::venue::{BrokerOrder, BrokerOrderState, VenueUpdate};

/// The broker's reason for refusing a request, or `None` when it did not refuse.
fn refusal(outcome: &RithmicRequestOutcome) -> Option<String> {
    let RithmicRequestOutcome::Rejected { code, reason } = outcome else {
        return None;
    };
    Some(
        reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map_or_else(
                || format!("Rithmic refused the request (code {code})"),
                str::to_string,
            ),
    )
}

/// The broker's explanation for an order that was refused or ended without filling, the
/// first non-blank text in order of preference.
fn state_reason(state: BrokerOrderState, texts: [Option<&String>; 2]) -> Option<String> {
    if !matches!(
        state,
        BrokerOrderState::Rejected
            | BrokerOrderState::CancelRejected
            | BrokerOrderState::Cancelled
            | BrokerOrderState::Expired
    ) {
        return None;
    }
    texts
        .into_iter()
        .flatten()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())
        .map(str::to_string)
}

const fn is_refusal(state: BrokerOrderState) -> bool {
    matches!(
        state,
        BrokerOrderState::Rejected | BrokerOrderState::CancelRejected
    )
}

impl Session<'_> {
    pub(super) fn on_order(&mut self, message: DecodedOrderMessage) -> Result<(), SessionEnd> {
        match message {
            DecodedOrderMessage::OrderNotification(notification) => {
                self.on_order_notification(&notification)
            }
            DecodedOrderMessage::ExchangeOrderNotification(notification) => {
                self.on_exchange_notification(&notification)
            }
            DecodedOrderMessage::OrderCommand(reply) => self.on_command(
                reply.request,
                reply.user_tag.as_deref(),
                reply.basket_id.as_deref(),
                &reply.outcome,
            ),
            DecodedOrderMessage::PositionExit(reply) => {
                self.on_exit(&reply.outcome);
                Ok(())
            }
            DecodedOrderMessage::RequestComplete(completion) => self.on_completion(
                completion.request,
                completion.user_tag.as_deref(),
                completion.basket_id.as_deref(),
                &completion.outcome,
            ),
            DecodedOrderMessage::TradeRouteUpdate(route) => {
                self.on_route(route);
                Ok(())
            }
            DecodedOrderMessage::LoginInfo(_)
            | DecodedOrderMessage::Account(_)
            | DecodedOrderMessage::TradeRoute(_)
            | DecodedOrderMessage::BracketLevel(_)
            | DecodedOrderMessage::BracketUpdate(_)
            | DecodedOrderMessage::FillHistory(_) => Ok(()),
        }
    }

    pub(super) fn on_pnl(&mut self, message: DecodedPnlMessage) -> Result<(), SessionEnd> {
        match message {
            DecodedPnlMessage::Instrument(row) => self.on_position(&row),
            DecodedPnlMessage::Account(row) => self.on_balance(&row),
            DecodedPnlMessage::RequestComplete {
                request: RithmicRequestKind::PnlSnapshot,
                outcome,
            } => {
                if let Some(reconcile) = self.snapshot_reconcile() {
                    reconcile.positions_done(&outcome);
                }
                self.finish_reconcile()
            }
            DecodedPnlMessage::RequestComplete { .. } => Ok(()),
        }
    }

    fn snapshot_reconcile(&mut self) -> Option<&mut Reconcile> {
        self.reconcile
            .as_mut()
            .filter(|reconcile| reconcile.is_snapshotting())
    }

    fn replaying(&self, account: &str) -> bool {
        self.reconcile
            .as_ref()
            .is_some_and(|reconcile| reconcile.replays(account))
    }

    fn emit_order(
        &self,
        account: &str,
        order: Option<BrokerOrder>,
        state: BrokerOrderState,
        reason: Option<String>,
    ) -> Result<(), SessionEnd> {
        let Some(order) = order else {
            diagnostic!("Aeris broker venue skipped an order report Rithmic sent incompletely");
            return Ok(());
        };
        self.emit(
            account,
            VenueUpdate::Order {
                order,
                state,
                reason,
            },
        )
    }

    fn on_order_notification(
        &mut self,
        notification: &RithmicOrderNotification,
    ) -> Result<(), SessionEnd> {
        let account = notification.order.account_id.as_str();
        if !self.knows(account) {
            return Ok(());
        }
        if notification.order.is_snapshot {
            if let Some(reconcile) = self.snapshot_reconcile().filter(|r| r.collects(account)) {
                let working = translate::snapshot_working(Some(notification.notify_type), None);
                reconcile.order_row(&notification.order, working);
            }
            return Ok(());
        }
        if self.replaying(account) {
            return Ok(());
        }
        let Some(state) = translate::order_state(notification) else {
            return Ok(());
        };
        let reason = state_reason(
            state,
            [
                notification.order.text.as_ref(),
                notification.completion_reason.as_ref(),
            ],
        );
        self.emit_order(
            account,
            translate::broker_order(&notification.order),
            state,
            reason,
        )
    }

    /// Fills are reported live and during an execution replay; the order snapshot that
    /// follows a replay repeats past fills, which the replay already covered.
    fn on_exchange_notification(
        &mut self,
        notification: &RithmicExchangeOrderNotification,
    ) -> Result<(), SessionEnd> {
        let account = notification.order.account_id.as_str();
        if !self.knows(account) {
            return Ok(());
        }
        let replaying = self.replaying(account);
        if notification.fill.is_some() && (replaying || !notification.order.is_snapshot) {
            match translate::broker_fill(notification, self.context.wall_nanos()?) {
                Some(fill) => self.emit(account, VenueUpdate::Fill(fill))?,
                None => {
                    diagnostic!("Aeris broker venue skipped a fill Rithmic sent incompletely");
                }
            }
        }
        if notification.order.is_snapshot {
            if let Some(reconcile) = self.snapshot_reconcile().filter(|r| r.collects(account)) {
                let working = translate::snapshot_working(None, Some(notification));
                reconcile.order_row(&notification.order, working);
            }
            return Ok(());
        }
        if replaying {
            return Ok(());
        }
        let Some(state) = translate::exchange_state(notification) else {
            return Ok(());
        };
        let reason = state_reason(
            state,
            [
                notification.order.text.as_ref(),
                // The report type only names the state; it explains nothing about a cancel.
                notification
                    .report_type
                    .as_ref()
                    .filter(|_| is_refusal(state)),
            ],
        );
        self.emit_order(
            account,
            translate::broker_order(&notification.order),
            state,
            reason,
        )
    }

    /// Settles the command a reply answers; a refusal goes to the owner.
    fn on_command(
        &mut self,
        request: RithmicRequestKind,
        user_tag: Option<&str>,
        basket_id: Option<&str>,
        outcome: &RithmicRequestOutcome,
    ) -> Result<(), SessionEnd> {
        let Some(command) = Command::of(request) else {
            return Ok(());
        };
        let key = if command == Command::Place {
            user_tag
        } else {
            basket_id
        };
        let refused = refusal(outcome);
        let Some(pending) = self.pending.answered(command, key, refused.is_some()) else {
            return Ok(());
        };
        match refused {
            Some(reason) => self.emit(
                &pending.broker_account,
                VenueUpdate::Refused {
                    client_order_id: pending.client_order_id,
                    reason,
                },
            ),
            None => Ok(()),
        }
    }

    fn on_completion(
        &mut self,
        request: RithmicRequestKind,
        user_tag: Option<&str>,
        basket_id: Option<&str>,
        outcome: &RithmicRequestOutcome,
    ) -> Result<(), SessionEnd> {
        match request {
            RithmicRequestKind::NewOrder
            | RithmicRequestKind::BracketOrder
            | RithmicRequestKind::ModifyOrder
            | RithmicRequestKind::CancelOrder => {
                self.on_command(request, user_tag, basket_id, outcome)
            }
            RithmicRequestKind::ReplayExecutions => self.on_replayed(outcome),
            RithmicRequestKind::ShowOrders => {
                if let Some(reconcile) = self.snapshot_reconcile() {
                    reconcile.orders_done(outcome);
                }
                self.finish_reconcile()
            }
            RithmicRequestKind::UpdateTargetLevel | RithmicRequestKind::UpdateStopLevel => {
                self.on_level(outcome)
            }
            RithmicRequestKind::ExitPosition => {
                self.on_exit(outcome);
                Ok(())
            }
            RithmicRequestKind::CancelAllOrders => {
                if let Some(reason) = refusal(outcome) {
                    diagnostic!(
                        "Aeris broker venue: Rithmic refused to cancel all orders: {reason}"
                    );
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The replay finished; the account's order and position snapshots follow it.
    fn on_replayed(&mut self, outcome: &RithmicRequestOutcome) -> Result<(), SessionEnd> {
        let Some(reconcile) = self
            .reconcile
            .as_ref()
            .filter(|reconcile| reconcile.is_replaying())
        else {
            return Ok(());
        };
        if let Some(reason) = refusal(outcome) {
            diagnostic!("Aeris broker venue: Rithmic refused an execution replay: {reason}");
        }
        let (account, deadline) = (reconcile.account.clone(), reconcile.deadline);
        self.begin_snapshot(account, deadline)
    }

    fn on_level(&mut self, outcome: &RithmicRequestOutcome) -> Result<(), SessionEnd> {
        let Some(level) = self.pending.level.take() else {
            return Ok(());
        };
        if let Some(reason) = refusal(outcome) {
            diagnostic!("Aeris broker venue: Rithmic refused to move a bracket level: {reason}");
            return Ok(());
        }
        self.emit(
            &level.broker_account,
            VenueUpdate::BracketLevelMoved {
                entry_broker_order_id: level.entry,
                side: level.side,
                from_ticks: level.from_ticks,
                to_ticks: level.to_ticks,
            },
        )
    }

    fn on_exit(&mut self, outcome: &RithmicRequestOutcome) {
        if !std::mem::take(&mut self.pending.exit) {
            return;
        }
        if let Some(reason) = refusal(outcome) {
            diagnostic!("Aeris broker venue: Rithmic refused to exit a position: {reason}");
        }
    }

    fn on_route(&mut self, route: RithmicTradeRoute) {
        let same = |known: &RithmicTradeRoute| {
            known.fcm_id == route.fcm_id
                && known.ib_id == route.ib_id
                && known.exchange == route.exchange
                && known.trade_route == route.trade_route
        };
        if let Some(known) = self.routes.iter_mut().find(|known| same(known)) {
            *known = route;
        } else if self.routes.len() < MAXIMUM_ROUTES {
            self.routes.push(route);
        } else {
            diagnostic!("Aeris broker venue ignored a Rithmic trade route beyond its bound");
        }
    }

    /// Reports the reconcile's snapshot once both of its answers arrived. A snapshot that
    /// cannot be reported leaves the account unreconciled, so the owner keeps it closed.
    fn finish_reconcile(&mut self) -> Result<(), SessionEnd> {
        if !self.reconcile.as_ref().is_some_and(Reconcile::complete) {
            return Ok(());
        }
        let Some(reconcile) = self.reconcile.take() else {
            return Ok(());
        };
        let account = reconcile.account.clone();
        match reconcile.into_snapshot() {
            Ok(snapshot) => self.emit(&account, VenueUpdate::Snapshot(snapshot)),
            Err(reason) => {
                diagnostic!("Aeris broker venue could not reconcile an account: {reason}");
                Ok(())
            }
        }
    }

    fn on_position(&mut self, row: &RithmicInstrumentPnl) -> Result<(), SessionEnd> {
        let account = row.account_id.as_str();
        if !self.knows(account) {
            return Ok(());
        }
        if row.is_snapshot {
            if let Some(reconcile) = self.snapshot_reconcile().filter(|r| r.collects(account)) {
                reconcile.position_row(row);
            }
            return Ok(());
        }
        match translate::venue_position(row) {
            Some(position) => self.emit(account, VenueUpdate::Position(position)),
            None => Ok(()),
        }
    }

    fn on_balance(&self, row: &RithmicAccountPnl) -> Result<(), SessionEnd> {
        let account = row.account_id.as_str();
        if !self.knows(account) {
            return Ok(());
        }
        match row.account_balance.and_then(translate::money) {
            Some(balance) => self.emit(account, VenueUpdate::Balance { balance }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_carry_the_brokers_reason_or_its_code() {
        assert_eq!(refusal(&RithmicRequestOutcome::Accepted), None);
        assert_eq!(refusal(&RithmicRequestOutcome::NoData), None);
        assert_eq!(
            refusal(&RithmicRequestOutcome::Rejected {
                code: 3,
                reason: Some(" bad price ".into()),
            })
            .as_deref(),
            Some("bad price")
        );
        assert_eq!(
            refusal(&RithmicRequestOutcome::Rejected {
                code: 5,
                reason: Some(String::new()),
            })
            .as_deref(),
            Some("Rithmic refused the request (code 5)")
        );
    }

    #[test]
    fn refused_and_ended_orders_keep_the_brokers_explanation() {
        let text = "insufficient margin".to_string();
        let blank = "  ".to_string();
        assert_eq!(
            state_reason(BrokerOrderState::Rejected, [None, Some(&text)]).as_deref(),
            Some("insufficient margin")
        );
        let cancelled = "cancelled by the risk system".to_string();
        assert_eq!(
            state_reason(
                BrokerOrderState::Cancelled,
                [Some(&blank), Some(&cancelled)]
            )
            .as_deref(),
            Some("cancelled by the risk system"),
            "a blank text gives way to the next explanation"
        );
        assert_eq!(
            state_reason(BrokerOrderState::Expired, [Some(&text), None]).as_deref(),
            Some("insufficient margin")
        );
        assert_eq!(
            state_reason(BrokerOrderState::Accepted, [Some(&text), None]),
            None
        );
    }
}
