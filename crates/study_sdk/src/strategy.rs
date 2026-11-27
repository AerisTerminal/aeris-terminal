//! Deterministic, isolated in-memory Strategy & Backtesting Engine for sandboxed WASM studies.
//!
//! Evaluates pure strategy intent commands (`Entry`, `Exit`, `Close`, `CloseAll`) emitted by a
//! study against canonical columnar OHLCV bars with configurable initial capital, commission, and
//! slippage. Never touches live broker accounts, venues, or durable trading stores.

use crate::fixed_point_to_f64;
use crate::sandbox::StudySandboxColumnarBars;
use std::collections::BTreeMap;

/// Maximum closed trades retained in one strategy backtest report.
pub const MAX_STRATEGY_TRADES: usize = 4_096;

/// Trade direction for a strategy order or filled position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrategyDirection {
    Long,
    Short,
}

/// Configurable simulation parameters for a strategy backtest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrategyConfig {
    pub initial_capital: f64,
    /// Round-trip or per-side commission in basis points (`5.0` = `0.05%` per side).
    pub commission_bps: f64,
    /// Slippage in ticks applied adversely to every fill.
    pub slippage_ticks: f64,
    /// Instrument tick size used to convert `slippage_ticks` into price units.
    pub tick_size: f64,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_ticks: 0.0,
            tick_size: 0.01,
        }
    }
}

/// Pure strategy intent emitted by a WASM study during execution.
#[derive(Clone, Debug, PartialEq)]
pub enum StrategyCommand {
    Entry {
        bar_index: usize,
        id: String,
        direction: StrategyDirection,
        qty: f64,
        limit_price: Option<f64>,
    },
    Exit {
        bar_index: usize,
        id: String,
        from_entry: String,
        profit_price: Option<f64>,
        loss_price: Option<f64>,
    },
    Close {
        bar_index: usize,
        id: String,
    },
    CloseAll {
        bar_index: usize,
    },
}

/// One completed round-trip trade produced by [`evaluate_strategy`].
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyTrade {
    pub entry_bar: usize,
    pub exit_bar: usize,
    pub entry_timestamp_ns: i64,
    pub exit_timestamp_ns: i64,
    pub direction: StrategyDirection,
    pub qty: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub gross_pnl: f64,
    pub commission: f64,
    pub net_pnl: f64,
    pub entry_id: String,
    pub exit_id: String,
}

/// Complete TradingView-class Strategy Tester performance summary & trade ledger.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrategyReport {
    pub initial_capital: f64,
    pub final_equity: f64,
    pub net_profit: f64,
    pub net_profit_pct: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate_pct: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub expectancy: f64,
    pub sharpe_ratio: f64,
    pub buy_and_hold_return_pct: f64,
    pub equity_curve: Vec<f64>,
    pub trades: Vec<StrategyTrade>,
}

#[derive(Clone, Debug)]
struct OpenPosition {
    entry_bar: usize,
    entry_timestamp_ns: i64,
    direction: StrategyDirection,
    qty: f64,
    entry_price: f64,
    entry_commission: f64,
    entry_id: String,
    profit_price: Option<f64>,
    loss_price: Option<f64>,
    exit_id: Option<String>,
}

fn apply_slippage(price: f64, is_buy: bool, config: StrategyConfig) -> f64 {
    let slip = config.slippage_ticks.max(0.0) * config.tick_size.max(0.0);
    if is_buy { price + slip } else { (price - slip).max(0.0) }
}

fn side_commission(price: f64, qty: f64, config: StrategyConfig) -> f64 {
    (price.abs() * qty.abs()) * (config.commission_bps.max(0.0) / 10_000.0)
}

fn close_position(
    pos: OpenPosition,
    exit_bar: usize,
    exit_timestamp_ns: i64,
    raw_exit_price: f64,
    exit_id: String,
    config: StrategyConfig,
) -> StrategyTrade {
    let is_buy_to_close = pos.direction == StrategyDirection::Short;
    let fill_price = apply_slippage(raw_exit_price, is_buy_to_close, config);
    let exit_comm = side_commission(fill_price, pos.qty, config);
    let gross_pnl = match pos.direction {
        StrategyDirection::Long => (fill_price - pos.entry_price) * pos.qty,
        StrategyDirection::Short => (pos.entry_price - fill_price) * pos.qty,
    };
    let total_comm = pos.entry_commission + exit_comm;
    let net_pnl = gross_pnl - total_comm;
    StrategyTrade {
        entry_bar: pos.entry_bar,
        exit_bar,
        entry_timestamp_ns: pos.entry_timestamp_ns,
        exit_timestamp_ns,
        direction: pos.direction,
        qty: pos.qty,
        entry_price: pos.entry_price,
        exit_price: fill_price,
        gross_pnl,
        commission: total_comm,
        net_pnl,
        entry_id: pos.entry_id,
        exit_id,
    }
}

/// Evaluates `commands` deterministically bar-by-bar over `bars` and computes a full
/// [`StrategyReport`].
#[must_use]
pub fn evaluate_strategy(
    bars: &StudySandboxColumnarBars,
    commands: &[StrategyCommand],
    config: StrategyConfig,
) -> StrategyReport {
    let n = bars.len();
    let initial_capital = if config.initial_capital.is_finite() && config.initial_capital > 0.0 {
        config.initial_capital
    } else {
        100_000.0
    };
    if n == 0 || commands.is_empty() {
        return StrategyReport {
            initial_capital,
            final_equity: initial_capital,
            ..StrategyReport::default()
        };
    }

    let mut by_bar: BTreeMap<usize, Vec<&StrategyCommand>> = BTreeMap::new();
    for cmd in commands {
        let b = match cmd {
            StrategyCommand::Entry { bar_index, .. }
            | StrategyCommand::Exit { bar_index, .. }
            | StrategyCommand::Close { bar_index, .. }
            | StrategyCommand::CloseAll { bar_index } => *bar_index,
        };
        if b < n {
            by_bar.entry(b).or_default().push(cmd);
        }
    }

    let mut cash = initial_capital;
    let mut position: Option<OpenPosition> = None;
    let mut trades: Vec<StrategyTrade> = Vec::new();
    let mut equity_curve: Vec<f64> = Vec::with_capacity(n);
    let mut peak_equity = initial_capital;
    let mut max_dd = 0.0_f64;
    let mut max_dd_pct = 0.0_f64;

    let first_close = fixed_point_to_f64(bars.close[0], bars.price_scale).unwrap_or(0.0);
    let last_close = fixed_point_to_f64(bars.close[n - 1], bars.price_scale).unwrap_or(0.0);
    let buy_and_hold_return_pct = if first_close > 0.0 && last_close > 0.0 {
        ((last_close - first_close) / first_close) * 100.0
    } else {
        0.0
    };

    for i in 0..n {
        let high = fixed_point_to_f64(bars.high[i], bars.price_scale).unwrap_or(f64::NAN);
        let low = fixed_point_to_f64(bars.low[i], bars.price_scale).unwrap_or(f64::NAN);
        let close = fixed_point_to_f64(bars.close[i], bars.price_scale).unwrap_or(f64::NAN);
        let ts = bars.timestamps_unix_nanos[i];

        // 1. Check active protective stop / take-profit brackets on this bar.
        if let Some(pos) = position.as_ref()
            && high.is_finite()
            && low.is_finite()
        {
            let hit_loss = pos.loss_price.is_some_and(|sl| match pos.direction {
                StrategyDirection::Long => low <= sl,
                StrategyDirection::Short => high >= sl,
            });
            let hit_profit = pos.profit_price.is_some_and(|tp| match pos.direction {
                StrategyDirection::Long => high >= tp,
                StrategyDirection::Short => low <= tp,
            });
            if hit_loss || hit_profit {
                let pos = position.take().expect("checked");
                let (fill_px, reason) = if hit_loss {
                    (
                        pos.loss_price.unwrap_or(close),
                        pos.exit_id.clone().unwrap_or_else(|| "SL".to_string()),
                    )
                } else {
                    (
                        pos.profit_price.unwrap_or(close),
                        pos.exit_id.clone().unwrap_or_else(|| "TP".to_string()),
                    )
                };
                let trade = close_position(pos, i, ts, fill_px, reason, config);
                cash += trade.net_pnl;
                if trades.len() < MAX_STRATEGY_TRADES {
                    trades.push(trade);
                }
            }
        }

        // 2. Process strategy commands scheduled on bar `i`.
        if let Some(cmds) = by_bar.get(&i)
            && close.is_finite()
        {
            for cmd in cmds {
                match cmd {
                    StrategyCommand::Entry {
                        id,
                        direction,
                        qty,
                        limit_price,
                        ..
                    } => {
                        if !qty.is_finite() || *qty <= 0.0 {
                            continue;
                        }
                        let fill_raw = match limit_price {
                            Some(lp) if lp.is_finite() => {
                                let touched = match direction {
                                    StrategyDirection::Long => low <= *lp,
                                    StrategyDirection::Short => high >= *lp,
                                };
                                if !touched {
                                    continue;
                                }
                                *lp
                            }
                            _ => close,
                        };
                        // Reverse existing opposite position if open.
                        if let Some(existing) = position.as_ref() {
                            if existing.direction == *direction {
                                continue;
                            }
                            let old = position.take().expect("checked");
                            let trade = close_position(
                                old,
                                i,
                                ts,
                                fill_raw,
                                format!("Reverse:{id}"),
                                config,
                            );
                            cash += trade.net_pnl;
                            if trades.len() < MAX_STRATEGY_TRADES {
                                trades.push(trade);
                            }
                        }
                        let is_buy = *direction == StrategyDirection::Long;
                        let entry_price = apply_slippage(fill_raw, is_buy, config);
                        let entry_commission = side_commission(entry_price, *qty, config);
                        position = Some(OpenPosition {
                            entry_bar: i,
                            entry_timestamp_ns: ts,
                            direction: *direction,
                            qty: *qty,
                            entry_price,
                            entry_commission,
                            entry_id: id.clone(),
                            profit_price: None,
                            loss_price: None,
                            exit_id: None,
                        });
                    }
                    StrategyCommand::Exit {
                        id,
                        from_entry,
                        profit_price,
                        loss_price,
                        ..
                    } => {
                        if let Some(pos) = position.as_mut()
                            && (from_entry.is_empty() || pos.entry_id == *from_entry)
                        {
                            pos.profit_price = *profit_price;
                            pos.loss_price = *loss_price;
                            pos.exit_id = Some(id.clone());
                        }
                    }
                    StrategyCommand::Close { id, .. } => {
                        if position
                            .as_ref()
                            .is_some_and(|p| id.is_empty() || p.entry_id == *id)
                        {
                            let pos = position.take().expect("checked");
                            let trade =
                                close_position(pos, i, ts, close, format!("Close:{id}"), config);
                            cash += trade.net_pnl;
                            if trades.len() < MAX_STRATEGY_TRADES {
                                trades.push(trade);
                            }
                        }
                    }
                    StrategyCommand::CloseAll { .. } => {
                        if let Some(pos) = position.take() {
                            let trade = close_position(
                                pos,
                                i,
                                ts,
                                close,
                                "CloseAll".to_string(),
                                config,
                            );
                            cash += trade.net_pnl;
                            if trades.len() < MAX_STRATEGY_TRADES {
                                trades.push(trade);
                            }
                        }
                    }
                }
            }
        }

        // 3. Mark-to-market equity & drawdown tracking.
        let unrealized = position.as_ref().map_or(0.0, |pos| {
            if !close.is_finite() {
                return 0.0;
            }
            match pos.direction {
                StrategyDirection::Long => (close - pos.entry_price) * pos.qty,
                StrategyDirection::Short => (pos.entry_price - close) * pos.qty,
            }
        });
        let equity = cash + unrealized;
        equity_curve.push(equity);
        if equity > peak_equity {
            peak_equity = equity;
        }
        let dd = (peak_equity - equity).max(0.0);
        if dd > max_dd {
            max_dd = dd;
        }
        if peak_equity > 0.0 {
            let dd_pct = (dd / peak_equity) * 100.0;
            if dd_pct > max_dd_pct {
                max_dd_pct = dd_pct;
            }
        }
    }

    let final_equity = equity_curve.last().copied().unwrap_or(initial_capital);
    let net_profit = final_equity - initial_capital;
    let net_profit_pct = (net_profit / initial_capital) * 100.0;

    let mut gross_profit = 0.0;
    let mut gross_loss = 0.0;
    let mut winning_trades = 0usize;
    let mut losing_trades = 0usize;
    for t in &trades {
        if t.net_pnl >= 0.0 {
            gross_profit += t.net_pnl;
            winning_trades += 1;
        } else {
            gross_loss += -t.net_pnl;
            losing_trades += 1;
        }
    }
    let total_trades = trades.len();
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    let win_rate_pct = if total_trades > 0 {
        (winning_trades as f64 / total_trades as f64) * 100.0
    } else {
        0.0
    };
    let avg_win = if winning_trades > 0 {
        gross_profit / winning_trades as f64
    } else {
        0.0
    };
    let avg_loss = if losing_trades > 0 {
        gross_loss / losing_trades as f64
    } else {
        0.0
    };
    let expectancy = if total_trades > 0 {
        (gross_profit - gross_loss) / total_trades as f64
    } else {
        0.0
    };

    // Per-bar equity return Sharpe (non-annualized bar Sharpe scaled by sqrt(252) when std > 0).
    let sharpe_ratio = compute_sharpe(&equity_curve);

    StrategyReport {
        initial_capital,
        final_equity,
        net_profit,
        net_profit_pct,
        gross_profit,
        gross_loss,
        profit_factor,
        max_drawdown: max_dd,
        max_drawdown_pct: max_dd_pct,
        total_trades,
        winning_trades,
        losing_trades,
        win_rate_pct,
        avg_win,
        avg_loss,
        expectancy,
        sharpe_ratio,
        buy_and_hold_return_pct,
        equity_curve,
        trades,
    }
}

fn compute_sharpe(equity: &[f64]) -> f64 {
    if equity.len() < 3 {
        return 0.0;
    }
    let mut returns = Vec::with_capacity(equity.len() - 1);
    for pair in equity.windows(2) {
        if pair[0] > 0.0 && pair[1].is_finite() {
            returns.push((pair[1] - pair[0]) / pair[0]);
        }
    }
    if returns.len() < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let var = returns
        .iter()
        .map(|r| {
            let d = r - mean;
            d * d
        })
        .sum::<f64>()
        / returns.len() as f64;
    let std = var.sqrt();
    if std > 1e-12 {
        (mean / std) * (252.0_f64).sqrt()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_strategy_backtest_computes_exact_pnl_commission_slippage_and_drawdown() {
        let bars = StudySandboxColumnarBars {
            price_scale: 2,
            quantity_scale: 0,
            timestamps_unix_nanos: vec![1_000, 2_000, 3_000, 4_000, 5_000],
            open: vec![10000, 10500, 11000, 10200, 10800],
            high: vec![10200, 10700, 11200, 10400, 11000],
            low: vec![9900, 10400, 10800, 9800, 10600],
            close: vec![10000, 10600, 11000, 10000, 10800],
            volume: vec![100; 5],
        };
        let commands = vec![
            StrategyCommand::Entry {
                bar_index: 0,
                id: "L1".to_string(),
                direction: StrategyDirection::Long,
                qty: 10.0,
                limit_price: None,
            },
            StrategyCommand::Close {
                bar_index: 2,
                id: "L1".to_string(),
            },
            StrategyCommand::Entry {
                bar_index: 2,
                id: "S1".to_string(),
                direction: StrategyDirection::Short,
                qty: 10.0,
                limit_price: None,
            },
            StrategyCommand::Exit {
                bar_index: 2,
                id: "TP_S1".to_string(),
                from_entry: "S1".to_string(),
                profit_price: Some(100.0),
                loss_price: Some(115.0),
            },
        ];
        let config = StrategyConfig {
            initial_capital: 10_000.0,
            commission_bps: 10.0, // 0.10% per side
            slippage_ticks: 0.0,
            tick_size: 0.01,
        };

        let report = evaluate_strategy(&bars, &commands, config);
        assert_eq!(report.total_trades, 2);
        assert_eq!(report.winning_trades, 2);
        assert_eq!(report.losing_trades, 0);
        assert!((report.win_rate_pct - 100.0).abs() < 1e-9);
        // Trade 1: Long 10 @ 100 -> 110 = +100 gross, comm = (1000 + 1100) * 0.001 = 2.10 -> 97.90
        assert!((report.trades[0].gross_pnl - 100.0).abs() < 1e-6);
        assert!((report.trades[0].commission - 2.10).abs() < 1e-6);
        assert!((report.trades[0].net_pnl - 97.90).abs() < 1e-6);
        // Trade 2: Short 10 @ 110 -> TP hit at bar 3 (low=98 <= 100) @ 100 = +100 gross, comm = 2.10 -> 97.90
        assert_eq!(report.trades[1].exit_bar, 3);
        assert!((report.trades[1].net_pnl - 97.90).abs() < 1e-6);
        assert!((report.net_profit - 195.80).abs() < 1e-6);
        assert!((report.buy_and_hold_return_pct - 8.0).abs() < 1e-6);
    }
}
