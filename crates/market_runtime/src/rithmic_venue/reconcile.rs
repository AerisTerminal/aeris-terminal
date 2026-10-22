//! One account reconcile: executions replayed since a point in time, then the order and
//! position snapshots gathered into one venue snapshot. A snapshot that cannot describe
//! every open order and position exactly is not reported, so the owner never settles
//! state against a partial view.

use super::translate;
use aeris_rithmic_protocol_adapter::{
    RithmicInstrumentPnl, RithmicOrderDetails, RithmicRequestOutcome,
};
use aeris_trading::venue::{BrokerOrder, VenuePosition, VenueSnapshot};
use std::{collections::BTreeMap, time::Instant};

/// Most orders or positions one snapshot may describe.
const MAXIMUM_ROWS: usize = 1_024;

enum Phase {
    Replaying,
    Snapshot {
        orders_done: bool,
        positions_done: bool,
    },
}

struct Row {
    order: Option<BrokerOrder>,
    working: bool,
}

pub(super) struct Reconcile {
    pub(super) account: String,
    pub(super) deadline: Instant,
    phase: Phase,
    orders: BTreeMap<String, Row>,
    positions: BTreeMap<(String, String), Option<VenuePosition>>,
    failed: Option<String>,
}

impl Reconcile {
    const fn new(account: String, deadline: Instant, phase: Phase) -> Self {
        Self {
            account,
            deadline,
            phase,
            orders: BTreeMap::new(),
            positions: BTreeMap::new(),
            failed: None,
        }
    }

    pub(super) const fn replaying(account: String, deadline: Instant) -> Self {
        Self::new(account, deadline, Phase::Replaying)
    }

    pub(super) const fn snapshotting(account: String, deadline: Instant) -> Self {
        let phase = Phase::Snapshot {
            orders_done: false,
            positions_done: false,
        };
        Self::new(account, deadline, phase)
    }

    pub(super) fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    pub(super) const fn is_replaying(&self) -> bool {
        matches!(self.phase, Phase::Replaying)
    }

    pub(super) const fn is_snapshotting(&self) -> bool {
        matches!(self.phase, Phase::Snapshot { .. })
    }

    /// Whether the account's executions are being replayed, while order reports for it
    /// are left to the snapshot that follows.
    pub(super) fn replays(&self, account: &str) -> bool {
        self.account == account && self.is_replaying()
    }

    /// Whether snapshot rows for the account belong to this reconcile.
    pub(super) fn collects(&self, account: &str) -> bool {
        self.account == account && self.is_snapshotting()
    }

    /// Keeps the latest report of each order in the snapshot.
    pub(super) fn order_row(&mut self, details: &RithmicOrderDetails, working: bool) {
        if self.orders.len() >= MAXIMUM_ROWS && !self.orders.contains_key(&details.basket_id) {
            self.failed = Some("the order snapshot exceeds its bound".into());
            return;
        }
        self.orders.insert(
            details.basket_id.clone(),
            Row {
                order: translate::broker_order(details),
                working,
            },
        );
    }

    pub(super) fn position_row(&mut self, row: &RithmicInstrumentPnl) {
        let key = (row.exchange.clone(), row.symbol.clone());
        if self.positions.len() >= MAXIMUM_ROWS && !self.positions.contains_key(&key) {
            self.failed = Some("the position snapshot exceeds its bound".into());
            return;
        }
        self.positions.insert(key, translate::venue_position(row));
    }

    pub(super) fn orders_done(&mut self, outcome: &RithmicRequestOutcome) {
        self.answered(outcome, "orders");
        if let Phase::Snapshot { orders_done, .. } = &mut self.phase {
            *orders_done = true;
        }
    }

    pub(super) fn positions_done(&mut self, outcome: &RithmicRequestOutcome) {
        self.answered(outcome, "positions");
        if let Phase::Snapshot { positions_done, .. } = &mut self.phase {
            *positions_done = true;
        }
    }

    fn answered(&mut self, outcome: &RithmicRequestOutcome, snapshot: &str) {
        if let RithmicRequestOutcome::Rejected { .. } = outcome {
            self.failed = Some(format!(
                "Rithmic refused the {snapshot} snapshot: {outcome:?}"
            ));
        }
    }

    pub(super) const fn complete(&self) -> bool {
        matches!(
            self.phase,
            Phase::Snapshot {
                orders_done: true,
                positions_done: true,
            }
        )
    }

    /// The account's working orders and open positions.
    ///
    /// # Errors
    /// Refuses a snapshot that was rejected, exceeded its bound, or holds a working order
    /// or a position Rithmic reported without the fields that describe it.
    pub(super) fn into_snapshot(self) -> Result<VenueSnapshot, String> {
        if let Some(failed) = self.failed {
            return Err(failed);
        }
        let orders = self
            .orders
            .into_values()
            .filter(|row| row.working)
            .map(|row| {
                row.order
                    .ok_or_else(|| "a working order was reported incompletely".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let positions = self
            .positions
            .into_values()
            .filter(|position| {
                position
                    .as_ref()
                    .is_none_or(|position| position.net_quantity.units() != 0)
            })
            .map(|position| {
                position.ok_or_else(|| "a position was reported incompletely".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(VenueSnapshot { orders, positions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_rithmic_protocol_adapter::{
        RithmicAccountRef, RithmicDecimal, RithmicPositionQuantities, RithmicReportedPriceType,
        RithmicReportedSide,
    };

    fn details(basket: &str, side: Option<RithmicReportedSide>) -> RithmicOrderDetails {
        RithmicOrderDetails {
            user_tag: None,
            is_snapshot: true,
            basket_id: basket.into(),
            original_basket_id: None,
            linked_basket_ids: None,
            bracket_type: None,
            account_id: RithmicAccountRef::new("account-1"),
            symbol: "ESZ5".into(),
            exchange: "CME".into(),
            trade_route: None,
            exchange_order_id: None,
            status: None,
            side,
            quantity: Some(1),
            price: RithmicDecimal::try_new(500_000, 2).ok(),
            trigger_price: None,
            price_type: Some(RithmicReportedPriceType::Limit),
            duration: None,
            placement: None,
            average_fill_price: None,
            total_fill_size: Some(0),
            total_unfilled_size: Some(1),
            text: None,
            timestamp: None,
        }
    }

    fn position(symbol: &str, net: Option<i32>) -> RithmicInstrumentPnl {
        RithmicInstrumentPnl {
            is_snapshot: true,
            account_id: RithmicAccountRef::new("account-1"),
            symbol: symbol.into(),
            exchange: "CME".into(),
            quantities: RithmicPositionQuantities {
                net_quantity: net,
                ..RithmicPositionQuantities::default()
            },
            average_open_fill_price: RithmicDecimal::try_new(500_000, 2).ok(),
            open_position_pnl: None,
            closed_position_pnl: None,
            day_open_pnl: None,
            day_closed_pnl: None,
            day_pnl: None,
            timestamp: None,
        }
    }

    fn deadline() -> Instant {
        Instant::now() + std::time::Duration::from_mins(1)
    }

    #[test]
    fn a_snapshot_reports_working_orders_and_open_positions_once_both_answers_arrive() {
        let reconcile = Reconcile::replaying("account-1".into(), deadline());
        assert!(reconcile.replays("account-1") && !reconcile.collects("account-1"));
        let mut reconcile = Reconcile::snapshotting(reconcile.account, reconcile.deadline);
        assert!(reconcile.collects("account-1") && !reconcile.collects("account-2"));
        reconcile.order_row(&details("working", Some(RithmicReportedSide::Buy)), true);
        reconcile.order_row(&details("done", Some(RithmicReportedSide::Buy)), true);
        reconcile.order_row(&details("done", None), false);
        reconcile.position_row(&position("ESZ5", Some(2)));
        reconcile.position_row(&position("NQZ5", Some(0)));
        reconcile.orders_done(&RithmicRequestOutcome::Accepted);
        assert!(!reconcile.complete());
        reconcile.positions_done(&RithmicRequestOutcome::NoData);
        assert!(reconcile.complete());
        let snapshot = reconcile.into_snapshot().expect("snapshot");
        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(snapshot.orders[0].broker_order_id, "working");
        assert_eq!(snapshot.positions.len(), 1);
        assert_eq!(snapshot.positions[0].net_quantity.units(), 2);
    }

    #[test]
    fn incomplete_or_refused_snapshots_are_not_reported() {
        let mut unreadable = Reconcile::snapshotting("account-1".into(), deadline());
        unreadable.order_row(&details("working", None), true);
        assert!(unreadable.into_snapshot().is_err());
        let mut unknown = Reconcile::snapshotting("account-1".into(), deadline());
        unknown.position_row(&position("ESZ5", None));
        assert!(unknown.into_snapshot().is_err());
        let mut refused = Reconcile::snapshotting("account-1".into(), deadline());
        refused.orders_done(&RithmicRequestOutcome::Rejected {
            code: 3,
            reason: None,
        });
        assert!(refused.into_snapshot().is_err());
    }
}
