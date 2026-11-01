//! Report statistics of closed trades: exact fixed-point totals per source and currency, the
//! cumulative P&L and drawdown curves, daily totals and the instrument, side, weekday and hour
//! breakdowns. Trades whose P&L the owner does not know are counted and listed but left out of
//! every total.

use super::ReportError;
use super::period::ReportZone;
use aeris_trading::{AccountEnvironment, OrderSide, TradingAccount, TradingAccountId};
use aeris_trading_runtime::{TradeLeg, TradeRoundTrip};
use chrono::{Datelike, NaiveDate, Timelike};
use std::collections::BTreeMap;

/// Where a section's fills came from.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ReportSource {
    /// Practice and paper accounts filled by the Aeris simulator.
    Simulated,
    /// Demo and live accounts whose fills the broker reported.
    Broker,
}

impl ReportSource {
    const fn of(environment: AccountEnvironment) -> Self {
        match environment {
            AccountEnvironment::Simulated => Self::Simulated,
            AccountEnvironment::Demo | AccountEnvironment::Live => Self::Broker,
        }
    }
}

/// One closed trade as the report lists it, with its P&L at the group scale.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReportTrade {
    pub(super) account: String,
    pub(super) symbol: String,
    pub(super) side: OrderSide,
    pub(super) entry: Option<TradeLeg>,
    pub(super) exit: TradeLeg,
    pub(super) pnl: Option<i128>,
}

/// Trades and P&L of one slice of a group, such as one instrument or one weekday.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Breakdown {
    pub(super) trades: usize,
    pub(super) wins: usize,
    pub(super) net: i128,
}

impl Breakdown {
    fn add(&mut self, pnl: i128) -> Result<(), ReportError> {
        self.trades += 1;
        if pnl > 0 {
            self.wins += 1;
        }
        self.net = self
            .net
            .checked_add(pnl)
            .ok_or(ReportError::AmountOverflow)?;
        Ok(())
    }

    pub(super) fn win_rate(&self) -> Option<f64> {
        ratio(self.wins, self.trades)
    }
}

/// The headline statistics of one group.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Summary {
    /// Every closed trade, including those with unknown P&L.
    pub(super) trades: usize,
    pub(super) unknown_pnl: usize,
    pub(super) wins: usize,
    pub(super) losses: usize,
    pub(super) net: i128,
    pub(super) gross_profit: i128,
    /// A non-negative magnitude.
    pub(super) gross_loss: i128,
    pub(super) largest_win: i128,
    /// A non-positive amount.
    pub(super) largest_loss: i128,
    /// Largest fall of the cumulative P&L below its running peak, as a non-negative magnitude.
    pub(super) max_drawdown: i128,
    pub(super) longest_win_streak: usize,
    pub(super) longest_loss_streak: usize,
}

impl Summary {
    pub(super) const fn known(&self) -> usize {
        self.trades - self.unknown_pnl
    }

    pub(super) fn win_rate(&self) -> Option<f64> {
        ratio(self.wins, self.known())
    }

    /// Gross profit over gross loss; `None` without any losing trade.
    pub(super) fn profit_factor(&self) -> Option<f64> {
        (self.gross_loss > 0).then(|| to_f64(self.gross_profit) / to_f64(self.gross_loss))
    }

    pub(super) fn average_win(&self) -> Option<i128> {
        average(self.gross_profit, self.wins)
    }

    /// A non-positive amount.
    pub(super) fn average_loss(&self) -> Option<i128> {
        average(self.gross_loss, self.losses).map(|loss| -loss)
    }

    /// Average P&L per trade with known P&L.
    pub(super) fn expectancy(&self) -> Option<i128> {
        average(self.net, self.known())
    }
}

/// The cumulative P&L and its drawdown after one trade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CurvePoint {
    pub(super) cumulative: i128,
    /// A non-negative magnitude below the running peak.
    pub(super) drawdown: i128,
}

/// All trades of one source in one currency, with every amount at `scale`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CurrencyGroup {
    pub(super) source: ReportSource,
    pub(super) currency: String,
    pub(super) scale: u8,
    /// Oldest exit first.
    pub(super) trades: Vec<ReportTrade>,
    pub(super) summary: Summary,
    pub(super) curve: Vec<CurvePoint>,
    pub(super) daily: Vec<(NaiveDate, Breakdown)>,
    /// Ordered by display symbol.
    pub(super) instruments: Vec<(String, Breakdown)>,
    pub(super) long: Breakdown,
    pub(super) short: Breakdown,
    /// Monday first.
    pub(super) weekdays: [Breakdown; 7],
    pub(super) hours: [Breakdown; 24],
}

/// One account of the report scope, listed even without trades in the period.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AccountLine {
    pub(super) name: String,
    pub(super) environment: AccountEnvironment,
    pub(super) currency: String,
    pub(super) scale: u8,
    pub(super) trades: usize,
    pub(super) net: i128,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReportSummary {
    /// Simulated groups first, then broker groups; each ordered by currency.
    pub(super) groups: Vec<CurrencyGroup>,
    pub(super) accounts: Vec<AccountLine>,
}

pub(super) fn build_report(
    trades: &[TradeRoundTrip],
    accounts: &[TradingAccount],
    symbols: &BTreeMap<aeris_instruments::InstrumentId, String>,
    zone: ReportZone,
) -> Result<ReportSummary, ReportError> {
    let account_of = |id: &TradingAccountId| accounts.iter().find(|account| account.id == *id);
    let group_key = |account: &TradingAccount| {
        (
            ReportSource::of(account.environment),
            account.currency.clone(),
        )
    };
    // Every amount of a group shares the finest scale among its accounts and P&L values, so
    // sums stay exact.
    let mut scales = BTreeMap::<(ReportSource, String), u8>::new();
    for account in accounts {
        let scale = scales.entry(group_key(account)).or_default();
        *scale = (*scale).max(account.currency_scale);
    }
    for trade in trades {
        if let (Some(account), Some(pnl)) = (account_of(&trade.account_id), trade.final_pnl) {
            let scale = scales.entry(group_key(account)).or_default();
            *scale = (*scale).max(pnl.scale());
        }
    }
    let mut grouped = BTreeMap::<(ReportSource, String), Vec<(&TradeRoundTrip, i64)>>::new();
    for trade in trades {
        let (Some(account), Some(exit)) = (account_of(&trade.account_id), trade.exit) else {
            continue;
        };
        grouped
            .entry(group_key(account))
            .or_default()
            .push((trade, exit.last_unix_nanos));
    }
    let mut groups = Vec::with_capacity(grouped.len());
    for ((source, currency), mut members) in grouped {
        members.sort_by(|left, right| {
            left.1.cmp(&right.1).then_with(|| {
                left.0
                    .exit_fill_id
                    .as_ref()
                    .map(aeris_trading::FillId::as_str)
                    .cmp(
                        &right
                            .0
                            .exit_fill_id
                            .as_ref()
                            .map(aeris_trading::FillId::as_str),
                    )
            })
        });
        let scale = scales
            .get(&(source, currency.clone()))
            .copied()
            .unwrap_or_default();
        let report_trades = members
            .iter()
            .filter_map(|(trade, _)| {
                let exit = trade.exit?;
                Some(Ok(ReportTrade {
                    account: account_of(&trade.account_id).map_or_else(
                        || trade.account_id.as_str().to_string(),
                        |account| account.display_name.clone(),
                    ),
                    symbol: symbols
                        .get(&trade.instrument_id)
                        .cloned()
                        .unwrap_or_else(|| trade.instrument_id.as_str().to_string()),
                    side: trade.side,
                    entry: trade.entry,
                    exit,
                    pnl: match trade.final_pnl {
                        Some(pnl) => match at_scale(pnl, scale) {
                            Some(units) => Some(units),
                            None => return Some(Err(ReportError::AmountOverflow)),
                        },
                        None => None,
                    },
                }))
            })
            .collect::<Result<Vec<_>, _>>()?;
        groups.push(group_statistics(
            source,
            currency,
            scale,
            report_trades,
            zone,
        )?);
    }
    Ok(ReportSummary {
        groups,
        accounts: account_lines(trades, accounts, &scales)?,
    })
}

/// One line per reported account at its group's scale, simulated accounts first.
fn account_lines(
    trades: &[TradeRoundTrip],
    accounts: &[TradingAccount],
    scales: &BTreeMap<(ReportSource, String), u8>,
) -> Result<Vec<AccountLine>, ReportError> {
    let mut lines = Vec::with_capacity(accounts.len());
    for account in accounts {
        let key = (
            ReportSource::of(account.environment),
            account.currency.clone(),
        );
        let scale = scales.get(&key).copied().unwrap_or(account.currency_scale);
        let mut line = AccountLine {
            name: account.display_name.clone(),
            environment: account.environment,
            currency: account.currency.clone(),
            scale,
            trades: 0,
            net: 0,
        };
        for trade in trades.iter().filter(|trade| trade.account_id == account.id) {
            line.trades += 1;
            if let Some(pnl) = trade.final_pnl {
                let units = at_scale(pnl, scale).ok_or(ReportError::AmountOverflow)?;
                line.net = line
                    .net
                    .checked_add(units)
                    .ok_or(ReportError::AmountOverflow)?;
            }
        }
        lines.push(line);
    }
    lines.sort_by_key(|line| ReportSource::of(line.environment));
    Ok(lines)
}

fn group_statistics(
    source: ReportSource,
    currency: String,
    scale: u8,
    trades: Vec<ReportTrade>,
    zone: ReportZone,
) -> Result<CurrencyGroup, ReportError> {
    let mut summary = Summary {
        trades: trades.len(),
        ..Summary::default()
    };
    let mut curve = Vec::with_capacity(trades.len());
    let mut peak = 0_i128;
    let (mut win_streak, mut loss_streak) = (0_usize, 0_usize);
    let mut daily = BTreeMap::<NaiveDate, Breakdown>::new();
    let mut instruments = BTreeMap::<String, Breakdown>::new();
    let (mut long, mut short) = (Breakdown::default(), Breakdown::default());
    let mut weekdays: [Breakdown; 7] = Default::default();
    let mut hours: [Breakdown; 24] = Default::default();
    for trade in &trades {
        let Some(pnl) = trade.pnl else {
            summary.unknown_pnl += 1;
            continue;
        };
        let overflow = || ReportError::AmountOverflow;
        summary.net = summary.net.checked_add(pnl).ok_or_else(overflow)?;
        match pnl.cmp(&0) {
            std::cmp::Ordering::Greater => {
                summary.wins += 1;
                summary.gross_profit =
                    summary.gross_profit.checked_add(pnl).ok_or_else(overflow)?;
                summary.largest_win = summary.largest_win.max(pnl);
                win_streak += 1;
                loss_streak = 0;
            }
            std::cmp::Ordering::Less => {
                summary.losses += 1;
                summary.gross_loss = summary.gross_loss.checked_sub(pnl).ok_or_else(overflow)?;
                summary.largest_loss = summary.largest_loss.min(pnl);
                loss_streak += 1;
                win_streak = 0;
            }
            std::cmp::Ordering::Equal => {
                win_streak = 0;
                loss_streak = 0;
            }
        }
        summary.longest_win_streak = summary.longest_win_streak.max(win_streak);
        summary.longest_loss_streak = summary.longest_loss_streak.max(loss_streak);
        peak = peak.max(summary.net);
        let drawdown = peak.checked_sub(summary.net).ok_or_else(overflow)?;
        summary.max_drawdown = summary.max_drawdown.max(drawdown);
        curve.push(CurvePoint {
            cumulative: summary.net,
            drawdown,
        });
        let exit = zone.local(trade.exit.last_unix_nanos);
        daily.entry(exit.date()).or_default().add(pnl)?;
        instruments
            .entry(trade.symbol.clone())
            .or_default()
            .add(pnl)?;
        match trade.side {
            OrderSide::Buy => long.add(pnl)?,
            OrderSide::Sell => short.add(pnl)?,
        }
        let weekday = usize::try_from(exit.weekday().num_days_from_monday()).unwrap_or(0);
        if let Some(weekday) = weekdays.get_mut(weekday) {
            weekday.add(pnl)?;
        }
        if let Some(hour) = usize::try_from(exit.hour())
            .ok()
            .and_then(|hour| hours.get_mut(hour))
        {
            hour.add(pnl)?;
        }
    }
    Ok(CurrencyGroup {
        source,
        currency,
        scale,
        trades,
        summary,
        curve,
        daily: daily.into_iter().collect(),
        instruments: instruments.into_iter().collect(),
        long,
        short,
        weekdays,
        hours,
    })
}

fn at_scale(value: aeris_trading::FixedPoint, scale: u8) -> Option<i128> {
    let factor = 10_i128.checked_pow(u32::from(scale.checked_sub(value.scale())?))?;
    i128::from(value.units()).checked_mul(factor)
}

fn average(total: i128, count: usize) -> Option<i128> {
    let count = i128::try_from(count).ok().filter(|count| *count > 0)?;
    Some(total / count)
}

fn ratio(part: usize, whole: usize) -> Option<f64> {
    use num_traits::ToPrimitive as _;
    (whole > 0).then(|| part.to_f64().unwrap_or(0.0) / whole.to_f64().unwrap_or(1.0))
}

/// Approximate value for ratios and chart geometry only; totals stay exact.
pub(super) fn to_f64(value: i128) -> f64 {
    use num_traits::ToPrimitive as _;
    value.to_f64().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::super::collect::tests::closed_trade;
    use super::*;

    fn account(id: &str, environment: AccountEnvironment, currency: &str) -> TradingAccount {
        TradingAccount {
            id: TradingAccountId::try_new(id).expect("account"),
            display_name: format!("{id} name"),
            environment,
            venue_id: "fixture-venue".to_string(),
            broker_ref: None,
            currency: currency.to_string(),
            currency_scale: 2,
            starting_equity: None,
        }
    }

    fn report(trades: &[TradeRoundTrip], accounts: &[TradingAccount]) -> ReportSummary {
        build_report(
            trades,
            accounts,
            &BTreeMap::new(),
            ReportZone::new("Etc/UTC"),
        )
        .expect("report")
    }

    #[test]
    fn summary_statistics_are_exact_and_ignore_unknown_pnl() {
        let accounts = [account("practice", AccountEnvironment::Simulated, "USD")];
        // Exit order: +100.00, -40.00, unknown, +60.00, -80.00, -10.00.
        let trades = [
            closed_trade("practice", 10, Some(10_000)),
            closed_trade("practice", 20, Some(-4_000)),
            closed_trade("practice", 30, None),
            closed_trade("practice", 40, Some(6_000)),
            closed_trade("practice", 50, Some(-8_000)),
            closed_trade("practice", 60, Some(-1_000)),
        ];
        let summary = report(&trades, &accounts);
        let [group] = summary.groups.as_slice() else {
            panic!("one group");
        };
        let stats = &group.summary;
        assert_eq!((stats.trades, stats.unknown_pnl, stats.known()), (6, 1, 5));
        assert_eq!((stats.wins, stats.losses), (2, 3));
        assert_eq!(stats.net, 3_000);
        assert_eq!((stats.gross_profit, stats.gross_loss), (16_000, 13_000));
        assert_eq!((stats.largest_win, stats.largest_loss), (10_000, -8_000));
        assert_eq!(stats.average_win(), Some(8_000));
        assert_eq!(stats.average_loss(), Some(-4_333));
        assert_eq!(stats.expectancy(), Some(600));
        assert_eq!(stats.win_rate(), Some(0.4));
        assert!((stats.profit_factor().expect("factor") - 16.0 / 13.0).abs() < 1e-12);
        // Cumulative: 100, 60, 120, 40, 30; the peak 120 falls to 30.
        assert_eq!(stats.max_drawdown, 9_000);
        assert_eq!(
            group
                .curve
                .iter()
                .map(|point| point.cumulative)
                .collect::<Vec<_>>(),
            vec![10_000, 6_000, 12_000, 4_000, 3_000]
        );
        assert_eq!(
            (stats.longest_win_streak, stats.longest_loss_streak),
            (1, 2)
        );
        assert_eq!(group.trades.len(), 6, "unknown P&L trades are still listed");
        assert_eq!(group.long.trades, 5);
        assert_eq!(group.weekdays[3].trades, 5, "1970-01-01 was a Thursday");
        assert_eq!(group.hours[0].net, 3_000);
        assert_eq!(summary.accounts[0].trades, 6);
        assert_eq!(summary.accounts[0].net, 3_000);
    }

    #[test]
    fn simulated_and_broker_accounts_and_currencies_form_separate_groups() {
        let accounts = [
            account("broker", AccountEnvironment::Demo, "USD"),
            account("practice", AccountEnvironment::Simulated, "USD"),
            account("euro", AccountEnvironment::Simulated, "EUR"),
        ];
        let trades = [
            closed_trade("broker", 10, Some(500)),
            closed_trade("practice", 20, Some(700)),
            closed_trade("euro", 30, Some(-300)),
        ];
        let summary = report(&trades, &accounts);
        let keys = summary
            .groups
            .iter()
            .map(|group| (group.source, group.currency.as_str(), group.summary.net))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                (ReportSource::Simulated, "EUR", -300),
                (ReportSource::Simulated, "USD", 700),
                (ReportSource::Broker, "USD", 500),
            ]
        );
        assert_eq!(
            summary.accounts.last().map(|line| line.environment),
            Some(AccountEnvironment::Demo),
            "broker accounts are listed after simulated ones"
        );
    }

    #[test]
    fn values_with_different_scales_sum_exactly_at_the_finest_scale() {
        let accounts = [account("practice", AccountEnvironment::Simulated, "USD")];
        let mut fine = closed_trade("practice", 20, None);
        fine.final_pnl = Some(aeris_trading::FixedPoint::try_new(12_345, 4).expect("pnl"));
        let trades = [closed_trade("practice", 10, Some(100)), fine];
        let summary = report(&trades, &accounts);
        assert_eq!(summary.groups[0].scale, 4);
        assert_eq!(summary.groups[0].summary.net, 22_345);
    }
}
