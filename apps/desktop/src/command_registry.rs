//! Product command metadata shared by menus, keyboard bindings, and the command palette.

/// Stable command identifiers. Execution stays at the owning desktop surface/runtime boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CommandId {
    OpenPalette,
    ToggleContext,
    ToggleOrderBook,
    ToggleWatchlist,
    ChartCandles,
    ChartBars,
    ChartLine,
    ChartArea,
    ChartBaseline,
    ChartFootprint,
    Interval1Minute,
    Interval5Minutes,
    Interval15Minutes,
    Interval1Hour,
    Interval1Day,
    NewWorkspace,
    SplitHorizontal,
    SplitVertical,
    BuyMarket,
    SellMarket,
    CancelAll,
    FlattenAccount,
    KillSwitch,
}

impl CommandId {
    pub const ALL: [Self; 23] = [
        Self::OpenPalette,
        Self::ToggleContext,
        Self::ToggleOrderBook,
        Self::ToggleWatchlist,
        Self::ChartCandles,
        Self::ChartBars,
        Self::ChartLine,
        Self::ChartArea,
        Self::ChartBaseline,
        Self::ChartFootprint,
        Self::Interval1Minute,
        Self::Interval5Minutes,
        Self::Interval15Minutes,
        Self::Interval1Hour,
        Self::Interval1Day,
        Self::NewWorkspace,
        Self::SplitHorizontal,
        Self::SplitVertical,
        Self::BuyMarket,
        Self::SellMarket,
        Self::CancelAll,
        Self::FlattenAccount,
        Self::KillSwitch,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: &'static str,
    pub keywords: &'static str,
    pub chord: Option<&'static str>,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::OpenPalette,
        title: "Open command palette",
        keywords: "search commands",
        chord: Some("ctrl-k"),
    },
    CommandSpec {
        id: CommandId::ToggleContext,
        title: "Market context",
        keywords: "toggle show hide calendar macro energy cot agriculture fundamentals",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ToggleOrderBook,
        title: "Order book",
        keywords: "toggle show hide dom depth",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ToggleWatchlist,
        title: "Watchlist",
        keywords: "toggle show hide symbols",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ChartCandles,
        title: "Chart: Candles",
        keywords: "candlestick",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ChartBars,
        title: "Chart: Bars",
        keywords: "ohlc",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ChartLine,
        title: "Chart: Line",
        keywords: "close",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ChartArea,
        title: "Chart: Area",
        keywords: "filled",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ChartBaseline,
        title: "Chart: Baseline",
        keywords: "comparison",
        chord: None,
    },
    CommandSpec {
        id: CommandId::ChartFootprint,
        title: "Chart: Footprint",
        keywords: "order flow bid ask delta",
        chord: None,
    },
    CommandSpec {
        id: CommandId::Interval1Minute,
        title: "Interval: 1 minute",
        keywords: "1m",
        chord: None,
    },
    CommandSpec {
        id: CommandId::Interval5Minutes,
        title: "Interval: 5 minutes",
        keywords: "5m",
        chord: None,
    },
    CommandSpec {
        id: CommandId::Interval15Minutes,
        title: "Interval: 15 minutes",
        keywords: "15m",
        chord: None,
    },
    CommandSpec {
        id: CommandId::Interval1Hour,
        title: "Interval: 1 hour",
        keywords: "1h 60m",
        chord: None,
    },
    CommandSpec {
        id: CommandId::Interval1Day,
        title: "Interval: 1 day",
        keywords: "1d daily",
        chord: None,
    },
    CommandSpec {
        id: CommandId::NewWorkspace,
        title: "New workspace",
        keywords: "tab",
        chord: Some("ctrl-t"),
    },
    CommandSpec {
        id: CommandId::SplitHorizontal,
        title: "Split pane horizontally",
        keywords: "layout",
        chord: Some("ctrl-alt-h"),
    },
    CommandSpec {
        id: CommandId::SplitVertical,
        title: "Split pane vertically",
        keywords: "layout",
        chord: Some("ctrl-alt-v"),
    },
    CommandSpec {
        id: CommandId::BuyMarket,
        title: "Trading: Buy market",
        keywords: "order",
        chord: Some("ctrl-b"),
    },
    CommandSpec {
        id: CommandId::SellMarket,
        title: "Trading: Sell market",
        keywords: "order",
        chord: Some("ctrl-s"),
    },
    CommandSpec {
        id: CommandId::CancelAll,
        title: "Trading: Cancel all",
        keywords: "orders",
        chord: Some("ctrl-shift-x"),
    },
    CommandSpec {
        id: CommandId::FlattenAccount,
        title: "Trading: Flatten account",
        keywords: "close positions",
        chord: Some("ctrl-shift-f"),
    },
    CommandSpec {
        id: CommandId::KillSwitch,
        title: "Trading: Kill switch",
        keywords: "cancel flatten lock",
        chord: Some("ctrl-shift-k"),
    },
];

#[must_use]
/// Returns the metadata for a stable command identifier.
///
/// # Panics
/// Panics only when the compile-time registry is incomplete. The registry uniqueness test covers
/// every identifier used by product UI and key bindings.
pub fn command(id: CommandId) -> &'static CommandSpec {
    COMMANDS
        .iter()
        .find(|spec| spec.id == id)
        .expect("every command id is registered")
}

/// Returns at most `limit` deterministic case-insensitive substring matches.
#[must_use]
pub fn search(query: &str, limit: usize) -> Vec<&'static CommandSpec> {
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .filter(|spec| {
            query.is_empty()
                || spec.title.to_ascii_lowercase().contains(&query)
                || spec
                    .keywords
                    .split_whitespace()
                    .any(|keyword| keyword.contains(&query))
        })
        .take(limit)
        .collect()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Mnemonic {
    pub symbol: Option<String>,
    pub chart: Option<CommandId>,
    pub interval: Option<CommandId>,
}

/// Parses a bounded Bloomberg-style command such as `ES footprint 5m`.
#[must_use]
pub fn parse_mnemonic(value: &str) -> Option<Mnemonic> {
    let words = value.split_whitespace().take(4).collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    let mut parsed = Mnemonic::default();
    for word in words {
        let normalized = word.to_ascii_lowercase();
        let command = match normalized.as_str() {
            "candle" | "candles" | "candlestick" => Some(CommandId::ChartCandles),
            "bar" | "bars" | "ohlc" => Some(CommandId::ChartBars),
            "line" => Some(CommandId::ChartLine),
            "area" => Some(CommandId::ChartArea),
            "baseline" => Some(CommandId::ChartBaseline),
            "footprint" | "fp" => Some(CommandId::ChartFootprint),
            "1m" => Some(CommandId::Interval1Minute),
            "5m" => Some(CommandId::Interval5Minutes),
            "15m" => Some(CommandId::Interval15Minutes),
            "1h" | "60m" => Some(CommandId::Interval1Hour),
            "1d" | "daily" => Some(CommandId::Interval1Day),
            _ => None,
        };
        match command {
            Some(
                id @ (CommandId::ChartCandles
                | CommandId::ChartBars
                | CommandId::ChartLine
                | CommandId::ChartArea
                | CommandId::ChartBaseline
                | CommandId::ChartFootprint),
            ) => parsed.chart = Some(id),
            Some(id) => parsed.interval = Some(id),
            None if parsed.symbol.is_none()
                && word.len() <= 32
                && word.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '/' | ':')
                }) =>
            {
                parsed.symbol = Some(word.to_ascii_uppercase());
            }
            None => return None,
        }
    }
    (parsed.symbol.is_some() && (parsed.chart.is_some() || parsed.interval.is_some()))
        .then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_stable_unique_and_search_is_bounded() {
        let mut ids = std::collections::BTreeSet::new();
        assert!(COMMANDS.iter().all(|spec| ids.insert(spec.id)));
        assert_eq!(ids, CommandId::ALL.into_iter().collect());
        assert_eq!(search("chart", 3).len(), 3);
        assert_eq!(command(CommandId::ToggleContext).title, "Market context");
    }

    #[test]
    fn mnemonic_parses_symbol_chart_and_interval_without_guessing() {
        assert_eq!(
            parse_mnemonic("ES footprint 5m"),
            Some(Mnemonic {
                symbol: Some("ES".to_string()),
                chart: Some(CommandId::ChartFootprint),
                interval: Some(CommandId::Interval5Minutes)
            })
        );
        assert_eq!(parse_mnemonic("ES unknown 5m"), None);
    }
}
