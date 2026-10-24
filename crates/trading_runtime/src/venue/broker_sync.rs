//! Whether each broker account may take new orders, and the venue outcomes its user must
//! see. An account takes new orders only once a reconcile of the current venue generation
//! has settled it and while every contract it traded is accounted: a deal or position in an
//! instrument the owner cannot account blocks the account's new orders (flatten, exits and
//! cancels stay available) until a reconcile after the instrument registers accounts it.
//! A reverse places its opening order only after its exit has flattened the position.

use crate::{PlaceOrder, RecoveryNotice, RecoveryNoticeKind, TradingCore, VenueNotice};
use aeris_instruments::InstrumentId;
use aeris_trading::TradingAccountId;
use aeris_trading::venue::{OperationFailure, VenueOperation};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The newest venue notices the owner keeps for its clients.
pub const VENUE_NOTICE_LIMIT: usize = 32;
/// How long a reverse waits for its exit to flatten the position before it is dropped.
pub(crate) const REVERSE_FLAT_DEADLINE_NANOS: i64 = 30_000_000_000;

const RECONCILING: &str =
    "the broker account is being reconciled with the broker; try again shortly";

/// Owner memory of the broker venue's per-generation readiness, waiting reverses and recent
/// notices. Unresolved contracts are durable store state instead.
#[derive(Default)]
pub(crate) struct BrokerSync {
    /// Accounts a reconcile of the current venue generation has settled.
    reconciled: BTreeSet<TradingAccountId>,
    /// Unresolved contracts whose deal or position failed again since the account's last
    /// applied reconcile; a reconcile clears every other contract it can now account.
    failed_since_snapshot: BTreeSet<(TradingAccountId, InstrumentId)>,
    reverses: BTreeMap<(TradingAccountId, InstrumentId), PendingReverse>,
    notices: VecDeque<VenueNotice>,
    next_notice: u64,
}

/// A reverse whose exit was requested; its opening order waits for the flat position.
struct PendingReverse {
    entry: PlaceOrder,
    deadline_unix_nanos: i64,
}

impl BrokerSync {
    /// A new venue generation: every account must be reconciled again.
    pub(crate) fn attached(&mut self) {
        self.reconciled.clear();
        self.failed_since_snapshot.clear();
    }

    pub(crate) fn notices(&self) -> Vec<VenueNotice> {
        self.notices.iter().cloned().collect()
    }
}

impl TradingCore {
    /// Refuses a new order on a broker account the current generation has not reconciled
    /// or that holds unresolved contracts.
    pub(crate) fn require_broker_ready(&self, account_id: &TradingAccountId) -> Result<(), String> {
        if !self.broker_sync.reconciled.contains(account_id) {
            return Err(RECONCILING.into());
        }
        match self
            .state
            .unresolved_contracts
            .get(account_id)
            .filter(|contracts| !contracts.is_empty())
        {
            Some(contracts) => Err(format!(
                "new orders are blocked until the broker's {} position is accounted; \
                 flatten and cancel remain available",
                instrument_list(contracts.keys())
            )),
            None => Ok(()),
        }
    }

    /// The earliest unaccounted deal of the account, below which a reconcile must replay.
    pub(crate) fn unaccounted_deal_floor(&self, account_id: &TradingAccountId) -> Option<i64> {
        self.state
            .unresolved_contracts
            .get(account_id)?
            .values()
            .flatten()
            .copied()
            .min()
    }

    /// Records one venue outcome the user must see.
    pub(crate) fn notify_venue(&mut self, account_id: &TradingAccountId, detail: String) {
        aeris_observability::diagnostic!("broker venue notice: {detail}");
        self.broker_sync.next_notice = self.broker_sync.next_notice.saturating_add(1);
        if self.broker_sync.notices.len() == VENUE_NOTICE_LIMIT {
            self.broker_sync.notices.pop_front();
        }
        self.broker_sync.notices.push_back(VenueNotice {
            sequence: self.broker_sync.next_notice,
            account_id: account_id.clone(),
            observed_unix_nanos: self.command_now,
            detail,
        });
        self.mark_revision = self.mark_revision.saturating_add(1);
    }

    /// Records a contract the owner could not account. `first_deal_unix_nanos` is the
    /// execution time of the unaccounted deal, or `None` for a position report.
    pub(crate) fn record_unresolved_contract(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        first_deal_unix_nanos: Option<i64>,
        cause: &str,
    ) -> Result<(), String> {
        self.store
            .put_unresolved_contract(account_id, instrument_id, first_deal_unix_nanos)?;
        self.broker_sync
            .failed_since_snapshot
            .insert((account_id.clone(), instrument_id.clone()));
        let contracts = self
            .state
            .unresolved_contracts
            .entry(account_id.clone())
            .or_default();
        let known = contracts.contains_key(instrument_id);
        let earliest = contracts.entry(instrument_id.clone()).or_insert(None);
        *earliest = match (*earliest, first_deal_unix_nanos) {
            (Some(stored), Some(deal)) => Some(stored.min(deal)),
            (stored, deal) => stored.or(deal),
        };
        if !known {
            self.notify_venue(
                account_id,
                format!(
                    "The broker's {} position cannot be accounted ({cause}); new orders on \
                     this account are blocked until it is reconciled",
                    instrument_id.as_str()
                ),
            );
        }
        Ok(())
    }

    /// Settles the account after the venue's reconcile snapshot was applied: it is ready
    /// for the current generation, positions in `unregistered` instruments stay unresolved,
    /// and every other unresolved contract that did not fail again is accounted.
    pub(crate) fn settle_reconciled(
        &mut self,
        account_id: &TradingAccountId,
        unregistered: &[InstrumentId],
    ) -> Result<(), String> {
        for instrument_id in unregistered {
            self.record_unresolved_contract(
                account_id,
                instrument_id,
                None,
                "its instrument is not registered",
            )?;
        }
        let resolved = self
            .state
            .unresolved_contracts
            .get(account_id)
            .into_iter()
            .flat_map(BTreeMap::keys)
            .filter(|instrument_id| {
                self.state.instruments.contains_key(*instrument_id)
                    && !self
                        .broker_sync
                        .failed_since_snapshot
                        .contains(&(account_id.clone(), (*instrument_id).clone()))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.broker_sync
            .failed_since_snapshot
            .retain(|(account, _)| account != account_id);
        self.broker_sync.reconciled.insert(account_id.clone());
        if resolved.is_empty() {
            return Ok(());
        }
        self.store
            .delete_unresolved_contracts(account_id, &resolved)?;
        let remaining =
            self.state
                .unresolved_contracts
                .get_mut(account_id)
                .map_or(0, |contracts| {
                    contracts.retain(|instrument_id, _| !resolved.contains(instrument_id));
                    contracts.len()
                });
        if remaining == 0 {
            self.state.unresolved_contracts.remove(account_id);
        }
        self.notify_venue(
            account_id,
            format!(
                "The broker's {} position is accounted{}",
                instrument_list(resolved.iter()),
                if remaining == 0 {
                    "; new orders are available again"
                } else {
                    ""
                }
            ),
        );
        Ok(())
    }

    /// A newly registered instrument may account contracts that were unresolved: each
    /// account holding one is reconciled again.
    pub(crate) fn instrument_registered(&mut self, instrument_id: &InstrumentId) {
        self.broker_sync
            .failed_since_snapshot
            .retain(|(_, failed)| failed != instrument_id);
        if self.venue_outbound.is_none() {
            return;
        }
        let requests = self
            .state
            .unresolved_contracts
            .iter()
            .filter(|(_, contracts)| contracts.contains_key(instrument_id))
            .filter_map(|(account_id, _)| {
                let account = self.state.accounts.get(account_id)?;
                Some((account_id.clone(), self.reconcile_request(account)?))
            })
            .collect::<Vec<_>>();
        for (account_id, request) in requests {
            if let Err(error) = self.send_venue_request(request) {
                self.notify_venue(
                    &account_id,
                    format!(
                        "The account could not be reconciled after a contract registered: {error}"
                    ),
                );
            }
        }
    }

    /// Applies a venue operation that did not complete. An unanswered request leaves the
    /// venue state uncertain, so the account takes no new orders until a reconcile settles it.
    pub(crate) fn apply_operation_failure(
        &mut self,
        account_id: &TradingAccountId,
        operation: &VenueOperation,
        failure: OperationFailure,
        reason: &str,
    ) -> Result<(), String> {
        let subject = match operation {
            VenueOperation::OrderCommand { client_order_id } => {
                format!("the request for order {}", client_order_id.as_str())
            }
            VenueOperation::CancelAll => "the request to cancel every order".into(),
            VenueOperation::ExitPosition { instrument_id } => {
                format!("the {} position exit", instrument_id.as_str())
            }
            VenueOperation::MoveBracketLevel {
                side,
                current_ticks,
                new_ticks,
                ..
            } => format!(
                "moving the bracket {} from {current_ticks} to {new_ticks} ticks",
                side.as_str()
            ),
            VenueOperation::Reconcile => "the account reconcile".into(),
        };
        if let VenueOperation::ExitPosition { instrument_id } = operation {
            self.drop_reverse(
                account_id,
                instrument_id,
                "its exit did not complete, so no opening order was placed",
            )?;
        }
        let detail = match failure {
            OperationFailure::Refused => format!("The broker refused {subject}: {reason}"),
            OperationFailure::Unanswered if *operation == VenueOperation::Reconcile => format!(
                "The broker did not answer {subject}: {reason}; new orders stay blocked until \
                 the venue reconnects"
            ),
            OperationFailure::Unanswered => format!(
                "The broker did not answer {subject}: {reason}; the account is being reconciled"
            ),
        };
        self.notify_venue(account_id, detail);
        if failure == OperationFailure::Unanswered && *operation != VenueOperation::Reconcile {
            self.broker_sync.reconciled.remove(account_id);
            let request = self
                .state
                .accounts
                .get(account_id)
                .and_then(|account| self.reconcile_request(account));
            if let Some(request) = request
                && let Err(error) = self.send_venue_request(request)
            {
                self.notify_venue(
                    account_id,
                    format!("The account could not be reconciled: {error}"),
                );
            }
        }
        Ok(())
    }

    /// Holds a reverse's opening order until its exit flattens the position. The marker is
    /// durable so a restart can report a reverse it will not complete.
    pub(crate) fn hold_reverse(
        &mut self,
        entry: PlaceOrder,
        requested_unix_nanos: i64,
    ) -> Result<(), String> {
        self.store.put_pending_reverse(
            &entry.account_id,
            &entry.instrument_id,
            requested_unix_nanos,
        )?;
        self.broker_sync.reverses.insert(
            (entry.account_id.clone(), entry.instrument_id.clone()),
            PendingReverse {
                entry,
                deadline_unix_nanos: requested_unix_nanos
                    .saturating_add(REVERSE_FLAT_DEADLINE_NANOS),
            },
        );
        Ok(())
    }

    pub(crate) fn reverse_waiting(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> bool {
        self.broker_sync
            .reverses
            .contains_key(&(account_id.clone(), instrument_id.clone()))
    }

    /// Forgets a waiting reverse without placing its opening order.
    pub(crate) fn release_reverse(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> Result<bool, String> {
        let key = (account_id.clone(), instrument_id.clone());
        if !self.broker_sync.reverses.contains_key(&key) {
            return Ok(false);
        }
        self.store
            .delete_pending_reverse(account_id, instrument_id)?;
        self.broker_sync.reverses.remove(&key);
        Ok(true)
    }

    fn drop_reverse(
        &mut self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        why: &str,
    ) -> Result<(), String> {
        if self.release_reverse(account_id, instrument_id)? {
            self.notify_venue(
                account_id,
                format!(
                    "The {} reverse was cancelled: {why}",
                    instrument_id.as_str()
                ),
            );
        }
        Ok(())
    }

    /// Places the opening order of every reverse of the account whose position is now flat.
    pub(crate) fn complete_flat_reverses(
        &mut self,
        account_id: &TradingAccountId,
    ) -> Result<(), String> {
        let flat = self
            .broker_sync
            .reverses
            .keys()
            .filter(|(account, instrument)| {
                account == account_id && self.is_flat(account, instrument)
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in flat {
            let Some(reverse) = self.broker_sync.reverses.get(&key) else {
                continue;
            };
            let entry = PlaceOrder {
                submitted_unix_nanos: self.command_now,
                ..reverse.entry.clone()
            };
            self.release_reverse(&key.0, &key.1)?;
            if let Err(error) = self.place_broker_order(&entry) {
                self.notify_venue(
                    &key.0,
                    format!(
                        "The {} position closed, but the reverse's opening order was not \
                         placed: {error}",
                        key.1.as_str()
                    ),
                );
            }
        }
        Ok(())
    }

    /// The earliest deadline of a waiting reverse.
    pub(crate) fn next_reverse_deadline_unix_nanos(&self) -> Option<i64> {
        self.broker_sync
            .reverses
            .values()
            .map(|reverse| reverse.deadline_unix_nanos)
            .min()
    }

    /// Drops every reverse whose position did not flatten before its deadline.
    pub(crate) fn expire_due_reverses(&mut self) -> Result<(), String> {
        let due = self
            .broker_sync
            .reverses
            .iter()
            .filter(|(_, reverse)| reverse.deadline_unix_nanos <= self.command_now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for (account_id, instrument_id) in due {
            self.drop_reverse(
                &account_id,
                &instrument_id,
                "the position did not close within 30 seconds, so no opening order was placed",
            )?;
        }
        Ok(())
    }

    /// Reports each reverse an earlier process left waiting; its opening order is never
    /// placed after a restart.
    pub(crate) fn report_interrupted_reverses(&mut self) -> Result<(), String> {
        for (account_id, instrument_id) in self.store.take_pending_reverses()? {
            self.recovery_notices.push(RecoveryNotice {
                kind: RecoveryNoticeKind::InterruptedReverse,
                subject: format!("{} {}", account_id.as_str(), instrument_id.as_str()),
                detail: "Aeris stopped while this reverse waited for its exit; no opening order \
                         was placed. Check the position."
                    .into(),
            });
        }
        Ok(())
    }
}

fn instrument_list<'a>(instruments: impl Iterator<Item = &'a InstrumentId>) -> String {
    instruments
        .map(InstrumentId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}
