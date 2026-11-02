//! Closed-trade pages of the report's accounts, read newest first through the trading owner's
//! read-only history worker until the report period starts.

use super::period::ReportSpan;
use super::{InstrumentLabel, ReportError};
use aeris_trading_runtime::{TradeHistoryPage, TradeHistoryQuery, TradeRoundTrip};
use std::collections::BTreeMap;

/// One report reads at most this many 500-trade pages across all of its accounts, so a report
/// never holds more than 50,000 trades.
pub(super) const MAXIMUM_REPORT_PAGES: usize = 100;

/// The owner's paged closed-trade history.
pub(super) trait ClosedTradePages {
    fn page(&self, query: TradeHistoryQuery) -> Result<TradeHistoryPage, ReportError>;
}

impl ClosedTradePages for aeris_trading_runtime::TradingService {
    fn page(&self, query: TradeHistoryQuery) -> Result<TradeHistoryPage, ReportError> {
        self.trade_history(query).map_err(ReportError::History)
    }
}

/// The closed trades whose exit falls inside the span, newest first per account.
pub(super) struct CollectedTrades {
    pub(super) trades: Vec<TradeRoundTrip>,
    /// The instruments the read pages referenced.
    pub(super) instruments: BTreeMap<aeris_instruments::InstrumentId, InstrumentLabel>,
    /// The page bound stopped reading before the period start was reached.
    pub(super) truncated: bool,
}

pub(super) fn collect_closed_trades(
    pages: &impl ClosedTradePages,
    accounts: &[aeris_trading::TradingAccountId],
    span: ReportSpan,
) -> Result<CollectedTrades, ReportError> {
    let mut trades = Vec::new();
    let mut instruments = BTreeMap::new();
    let mut remaining_pages = MAXIMUM_REPORT_PAGES;
    let mut truncated = false;
    for account in accounts {
        let mut after = None;
        loop {
            if remaining_pages == 0 {
                truncated = true;
                break;
            }
            remaining_pages -= 1;
            let page = pages.page(TradeHistoryQuery {
                account_id: account.clone(),
                after,
                limit: Some(aeris_trading_runtime::MAXIMUM_HISTORY_PAGE),
            })?;
            for instrument in &page.instruments {
                instruments
                    .entry(instrument.instrument_id.clone())
                    .or_insert_with(|| InstrumentLabel::of(instrument));
            }
            let mut reached_start = false;
            for trade in page.trades {
                let Some(exit) = trade.exit.filter(|_| trade.closed) else {
                    continue;
                };
                if exit.last_unix_nanos < span.start_unix_nanos {
                    reached_start = true;
                    break;
                }
                if exit.last_unix_nanos < span.end_unix_nanos {
                    trades.push(trade);
                }
            }
            match page.next {
                Some(next) if !reached_start => after = Some(next),
                _ => break,
            }
        }
    }
    Ok(CollectedTrades {
        trades,
        instruments,
        truncated,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::cell::RefCell;

    pub(in super::super) fn closed_trade(
        account: &str,
        exit_seconds: i64,
        pnl_cents: Option<i64>,
    ) -> TradeRoundTrip {
        let leg = aeris_trading_runtime::TradeLeg {
            first_unix_nanos: exit_seconds * 1_000_000_000,
            last_unix_nanos: exit_seconds * 1_000_000_000,
            average_price: aeris_trading::FixedPoint::try_new(10_000, 2).expect("price"),
            quantity: aeris_trading::FixedPoint::try_new(1, 0).expect("quantity"),
        };
        TradeRoundTrip {
            account_id: aeris_trading::TradingAccountId::try_new(account).expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("fixture:ES")
                .expect("instrument"),
            exit_fill_id: Some(
                aeris_trading::FillId::try_new(format!("{account}-{exit_seconds}")).expect("fill"),
            ),
            side: aeris_trading::OrderSide::Buy,
            entry: Some(leg),
            exit: Some(leg),
            closed: true,
            final_pnl: pnl_cents
                .map(|cents| aeris_trading::FixedPoint::try_new(cents, 2).expect("pnl")),
            open_pnl: None,
        }
    }

    /// Serves fixed newest-first pages of `size` trades and records every query.
    struct FixturePages {
        trades: Vec<TradeRoundTrip>,
        size: usize,
        queries: RefCell<Vec<TradeHistoryQuery>>,
    }

    impl ClosedTradePages for FixturePages {
        fn page(&self, query: TradeHistoryQuery) -> Result<TradeHistoryPage, ReportError> {
            let offset = query.after.as_ref().map_or(0, |cursor| {
                usize::try_from(cursor.exit_unix_nanos).expect("fixture offset")
            });
            self.queries.borrow_mut().push(query.clone());
            let trades = self
                .trades
                .iter()
                .filter(|trade| trade.account_id == query.account_id)
                .skip(offset)
                .take(self.size)
                .cloned()
                .collect::<Vec<_>>();
            let next =
                (trades.len() == self.size).then(|| aeris_trading_runtime::TradeHistoryCursor {
                    exit_unix_nanos: i64::try_from(offset + self.size).expect("fixture offset"),
                    exit_fill_id: aeris_trading::FillId::try_new("cursor").expect("cursor"),
                });
            Ok(TradeHistoryPage {
                trades,
                instruments: Vec::new(),
                next,
            })
        }
    }

    fn span(start_seconds: i64, end_seconds: i64) -> ReportSpan {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("day");
        ReportSpan {
            start_unix_nanos: start_seconds * 1_000_000_000,
            end_unix_nanos: end_seconds * 1_000_000_000,
            first_day: day,
            last_day: day,
        }
    }

    fn account(id: &str) -> aeris_trading::TradingAccountId {
        aeris_trading::TradingAccountId::try_new(id).expect("account")
    }

    #[test]
    fn reading_stops_at_the_period_start_and_skips_trades_after_its_end() {
        let pages = FixturePages {
            trades: (0..10)
                .rev()
                .map(|second| closed_trade("a", second * 10, Some(100)))
                .collect(),
            size: 3,
            queries: RefCell::new(Vec::new()),
        };
        let collected =
            collect_closed_trades(&pages, &[account("a")], span(30, 80)).expect("collected");
        let exits = collected
            .trades
            .iter()
            .map(|trade| trade.exit.expect("exit").last_unix_nanos / 1_000_000_000)
            .collect::<Vec<_>>();
        assert_eq!(exits, vec![70, 60, 50, 40, 30]);
        assert!(!collected.truncated);
        assert_eq!(
            pages.queries.borrow().len(),
            3,
            "the page holding the first trade before the start ends the read"
        );
    }

    #[test]
    fn the_page_bound_marks_the_report_truncated() {
        let pages = FixturePages {
            trades: (0..(i64::try_from(MAXIMUM_REPORT_PAGES).expect("bound") + 5))
                .rev()
                .map(|second| closed_trade("a", second + 1, Some(1)))
                .collect(),
            size: 1,
            queries: RefCell::new(Vec::new()),
        };
        let collected = collect_closed_trades(
            &pages,
            &[account("a"), account("b")],
            span(0, i64::MAX / 1_000_000_000),
        )
        .expect("collected");
        assert!(collected.truncated);
        assert_eq!(collected.trades.len(), MAXIMUM_REPORT_PAGES);
        assert_eq!(pages.queries.borrow().len(), MAXIMUM_REPORT_PAGES);
    }

    #[test]
    fn every_account_is_read_and_open_trades_are_ignored() {
        let mut open = closed_trade("b", 50, None);
        open.closed = false;
        let pages = FixturePages {
            trades: vec![
                closed_trade("a", 60, Some(5)),
                open,
                closed_trade("b", 40, Some(-5)),
            ],
            size: 500,
            queries: RefCell::new(Vec::new()),
        };
        let collected = collect_closed_trades(&pages, &[account("a"), account("b")], span(0, 100))
            .expect("collected");
        assert_eq!(collected.trades.len(), 2);
        assert!(collected.trades.iter().all(|trade| trade.closed));
    }
}
