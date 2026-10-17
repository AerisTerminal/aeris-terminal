//! Bounded equity history: an in-memory ring of recent samples plus a durable trail that
//! downsamples minute samples to hourly. Samples are telemetry, not ledger facts: the cash
//! ledger stays the balance authority.

use std::collections::VecDeque;

use super::super::{EquitySample, FixedPoint, TradingAccountId, TradingCore, is_simulated};

/// The in-memory ring keeps the most recent samples of one account: a full day at one
/// sample per minute.
pub(crate) const EQUITY_RING_CAPACITY: usize = 1_440;

/// The equity timer cadence.
pub(crate) const EQUITY_SAMPLE_INTERVAL_NANOS: i64 = 60_000_000_000;

/// Pushes one sample onto the account ring, dropping the oldest beyond the bound. Samples
/// of the same account and timestamp collapse to the newest, mirroring the durable
/// primary key (account, at, resolution).
pub(crate) fn push_sample(ring: &mut VecDeque<EquitySample>, sample: EquitySample) {
    if let Some(last) = ring.back_mut()
        && last.at_unix_nanos == sample.at_unix_nanos
    {
        *last = sample;
        return;
    }
    ring.push_back(sample);
    while ring.len() > EQUITY_RING_CAPACITY {
        ring.pop_front();
    }
}

impl TradingCore {
    /// The current equity (balance plus unrealized) of one simulated account.
    pub(crate) fn account_equity(&self, account_id: &TradingAccountId) -> Option<FixedPoint> {
        let account = self.state.accounts.get(account_id)?;
        if !is_simulated(&self.state.accounts, account_id) {
            return None;
        }
        let mut realized = FixedPoint::try_new(0, account.currency_scale).ok()?;
        let mut unrealized = realized;
        for position in self
            .state
            .positions
            .values()
            .filter(|position| position.account_id == *account_id)
        {
            realized = realized
                .checked_add(
                    position
                        .realized_pnl
                        .exact_rescale(account.currency_scale)
                        .ok()?,
                )
                .ok()?;
            unrealized = unrealized
                .checked_add(
                    position
                        .unrealized_pnl
                        .exact_rescale(account.currency_scale)
                        .ok()?,
                )
                .ok()?;
        }
        let balance = self.simulated_balance(account, realized).ok()??;
        balance.checked_add(unrealized).ok()
    }

    /// Pushes one already-persisted sample into the account ring (session-boundary samples
    /// commit with the rollover transaction, so the ring update follows the store write).
    pub(crate) fn push_equity_sample(&mut self, sample: EquitySample) {
        push_sample(
            self.state
                .equity_rings
                .entry(sample.account_id.clone())
                .or_default(),
            sample,
        );
    }

    /// Records one equity sample for a fill in the account ring and returns it so the
    /// caller's committing transaction can persist it. Ring-only: no store write here, so
    /// a speculative quote plan that rolls back leaves no trace.
    pub(crate) fn record_equity_sample(
        &mut self,
        account_id: &TradingAccountId,
        at: i64,
    ) -> Option<EquitySample> {
        let equity = self.account_equity(account_id)?;
        let sample = EquitySample {
            account_id: account_id.clone(),
            at_unix_nanos: at,
            equity,
        };
        push_sample(
            self.state
                .equity_rings
                .entry(account_id.clone())
                .or_default(),
            sample.clone(),
        );
        Some(sample)
    }

    /// The 60 s equity timer: a sample only when equity changed since the last one. The
    /// sample at each risk session boundary is written by the rollover itself, inside the
    /// same transaction as the new session row.
    pub(crate) fn run_equity_timer(&mut self, now: i64) {
        let account_ids: Vec<TradingAccountId> = self
            .state
            .accounts
            .values()
            .filter(|account| is_simulated(&self.state.accounts, &account.id))
            .map(|account| account.id.clone())
            .collect();
        for account_id in account_ids {
            let Some(equity) = self.account_equity(&account_id) else {
                continue;
            };
            let changed = self
                .state
                .equity_rings
                .get(&account_id)
                .and_then(|ring| ring.back())
                .is_none_or(|last| last.equity != equity);
            if !changed {
                continue;
            }
            let sample = EquitySample {
                account_id: account_id.clone(),
                at_unix_nanos: now,
                equity,
            };
            if let Err(error) = self.store.put_equity_sample(&sample) {
                aeris_observability::diagnostic!("equity sample persistence failed: {error}");
                continue;
            }
            push_sample(
                self.state.equity_rings.entry(account_id).or_default(),
                sample,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equity_ring_holds_at_most_a_full_day_of_minute_samples() {
        let mut ring = VecDeque::new();
        let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
        for index in 0..1_440_i64 * 2 {
            push_sample(
                &mut ring,
                EquitySample {
                    account_id: account.clone(),
                    at_unix_nanos: index * EQUITY_SAMPLE_INTERVAL_NANOS,
                    equity: FixedPoint::try_new(index, 2).expect("equity"),
                },
            );
        }
        assert_eq!(ring.len(), EQUITY_RING_CAPACITY);
        assert_eq!(
            ring.front().expect("oldest").at_unix_nanos,
            1_440 * EQUITY_SAMPLE_INTERVAL_NANOS
        );
    }
}
