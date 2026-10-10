//! Product command metadata shared by menus, keyboard bindings, and the command palette.
//!
//! This table is the only source of desktop keyboard shortcuts. Keys the chart consumes belong
//! to Aeris Charts; [`validate_chords`] keeps every shortcut here off them.

use gpui::Keystroke;

/// Stable command identifiers. Execution stays at the owning desktop surface/runtime boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CommandId {
    OpenPalette,
    ToggleFullscreen,
    MinimizeWindow,
    ZoomWindow,
    CloseWindow,
    ToggleContext,
    ToggleOrderBook,
    ToggleTimeSales,
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
    SelectNextWorkspace,
    SelectPreviousWorkspace,
    MoveWorkspaceLeft,
    MoveWorkspaceRight,
    CloseWorkspace,
    SplitHorizontal,
    SplitVertical,
    ClosePane,
    ToggleOneClickTrading,
    BuyMarket,
    SellMarket,
    CancelAll,
    FlattenAccount,
    KillSwitch,
}

impl CommandId {
    pub const ALL: [Self; 35] = [
        Self::OpenPalette,
        Self::ToggleFullscreen,
        Self::MinimizeWindow,
        Self::ZoomWindow,
        Self::CloseWindow,
        Self::ToggleContext,
        Self::ToggleOrderBook,
        Self::ToggleTimeSales,
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
        Self::SelectNextWorkspace,
        Self::SelectPreviousWorkspace,
        Self::MoveWorkspaceLeft,
        Self::MoveWorkspaceRight,
        Self::CloseWorkspace,
        Self::SplitHorizontal,
        Self::SplitVertical,
        Self::ClosePane,
        Self::ToggleOneClickTrading,
        Self::BuyMarket,
        Self::SellMarket,
        Self::CancelAll,
        Self::FlattenAccount,
        Self::KillSwitch,
    ];

    #[must_use]
    pub const fn group(self) -> CommandGroup {
        match self {
            Self::OpenPalette
            | Self::ToggleFullscreen
            | Self::MinimizeWindow
            | Self::ZoomWindow
            | Self::CloseWindow => CommandGroup::General,
            Self::ToggleContext
            | Self::ToggleOrderBook
            | Self::ToggleTimeSales
            | Self::ToggleWatchlist => CommandGroup::Panels,
            Self::ChartCandles
            | Self::ChartBars
            | Self::ChartLine
            | Self::ChartArea
            | Self::ChartBaseline
            | Self::ChartFootprint
            | Self::Interval1Minute
            | Self::Interval5Minutes
            | Self::Interval15Minutes
            | Self::Interval1Hour
            | Self::Interval1Day => CommandGroup::Chart,
            Self::NewWorkspace
            | Self::SelectNextWorkspace
            | Self::SelectPreviousWorkspace
            | Self::MoveWorkspaceLeft
            | Self::MoveWorkspaceRight
            | Self::CloseWorkspace => CommandGroup::Workspaces,
            Self::SplitHorizontal | Self::SplitVertical | Self::ClosePane => CommandGroup::Panes,
            Self::ToggleOneClickTrading
            | Self::BuyMarket
            | Self::SellMarket
            | Self::CancelAll
            | Self::FlattenAccount
            | Self::KillSwitch => CommandGroup::Trading,
        }
    }
}

/// The heading a command is listed under, in display order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CommandGroup {
    Trading,
    General,
    Workspaces,
    Panes,
    Panels,
    Chart,
}

impl CommandGroup {
    pub const ALL: [Self; 6] = [
        Self::Trading,
        Self::General,
        Self::Workspaces,
        Self::Panes,
        Self::Panels,
        Self::Chart,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Trading => "Trading",
            Self::General => "General",
            Self::Workspaces => "Workspaces",
            Self::Panes => "Panes",
            Self::Panels => "Panels",
            Self::Chart => "Chart",
        }
    }
}

/// Every command that has a shortcut, grouped under its heading in registry order. Groups
/// without a shortcut are left out.
#[must_use]
pub fn shortcut_sections() -> Vec<(CommandGroup, Vec<&'static CommandSpec>)> {
    CommandGroup::ALL
        .into_iter()
        .filter_map(|group| {
            let commands = COMMANDS
                .iter()
                .filter(|spec| spec.id.group() == group && !spec.chords.is_empty())
                .collect::<Vec<_>>();
            (!commands.is_empty()).then_some((group, commands))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: &'static str,
    pub keywords: &'static str,
    /// GPUI keystrokes bound to the command, the displayed shortcut first.
    pub chords: &'static [&'static str],
}

impl CommandSpec {
    /// The title without its group prefix, for lists that already show the group as a heading
    /// (`Buy market` under Trading).
    #[must_use]
    pub fn short_title(&self) -> &'static str {
        let title = self.title;
        title
            .strip_prefix(self.id.group().label())
            .and_then(|rest| rest.strip_prefix(": "))
            .unwrap_or(title)
    }

    /// The shortcut shown next to the command, written for people (`Ctrl+Shift+K`).
    #[must_use]
    pub fn shortcut_label(&self) -> Option<String> {
        self.chords.first().map(|chord| chord_label(chord))
    }
}

/// Writes a GPUI chord such as `ctrl-shift-pageup` as `Ctrl+Shift+Page Up`.
#[must_use]
pub fn chord_label(chord: &str) -> String {
    chord_keys(chord).join("+")
}

/// The keys of a GPUI chord as people write them: `ctrl-shift-pageup` is `Ctrl`, `Shift`,
/// `Page Up`.
#[must_use]
pub fn chord_keys(chord: &str) -> Vec<String> {
    chord
        .split('-')
        .map(|part| match part {
            "ctrl" => "Ctrl".to_string(),
            "shift" => "Shift".to_string(),
            "alt" => "Alt".to_string(),
            "pageup" => "Page Up".to_string(),
            "pagedown" => "Page Down".to_string(),
            key => {
                let mut characters = key.chars();
                characters.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(characters).collect()
                })
            }
        })
        .collect()
}

const fn spec(
    id: CommandId,
    title: &'static str,
    keywords: &'static str,
    chords: &'static [&'static str],
) -> CommandSpec {
    CommandSpec {
        id,
        title,
        keywords,
        chords,
    }
}

pub const COMMANDS: &[CommandSpec] = &[
    spec(
        CommandId::OpenPalette,
        "Open command palette",
        "search commands",
        &["ctrl-k"],
    ),
    spec(
        CommandId::ToggleFullscreen,
        "Toggle fullscreen",
        "window full screen",
        &["f11", "alt-enter"],
    ),
    spec(
        CommandId::MinimizeWindow,
        "Minimize window",
        "window",
        &["alt-f9"],
    ),
    spec(
        CommandId::ZoomWindow,
        "Maximize or restore window",
        "window zoom",
        &["alt-f10"],
    ),
    spec(
        CommandId::CloseWindow,
        "Close window",
        "window quit exit",
        &["alt-f4"],
    ),
    spec(
        CommandId::ToggleContext,
        "Market context",
        "toggle show hide calendar macro energy cot agriculture fundamentals",
        &[],
    ),
    spec(
        CommandId::ToggleOrderBook,
        "Order book",
        "toggle show hide dom depth",
        &[],
    ),
    spec(
        CommandId::ToggleTimeSales,
        "Time & Sales",
        "toggle show hide tape trades prints",
        &[],
    ),
    spec(
        CommandId::ToggleWatchlist,
        "Watchlist",
        "toggle show hide symbols",
        &[],
    ),
    spec(
        CommandId::ChartCandles,
        "Chart: Candles",
        "candlestick",
        &[],
    ),
    spec(CommandId::ChartBars, "Chart: Bars", "ohlc", &[]),
    spec(CommandId::ChartLine, "Chart: Line", "close", &[]),
    spec(CommandId::ChartArea, "Chart: Area", "filled", &[]),
    spec(
        CommandId::ChartBaseline,
        "Chart: Baseline",
        "comparison",
        &[],
    ),
    spec(
        CommandId::ChartFootprint,
        "Chart: Footprint",
        "order flow bid ask delta",
        &[],
    ),
    spec(CommandId::Interval1Minute, "Interval: 1 minute", "1m", &[]),
    spec(
        CommandId::Interval5Minutes,
        "Interval: 5 minutes",
        "5m",
        &[],
    ),
    spec(
        CommandId::Interval15Minutes,
        "Interval: 15 minutes",
        "15m",
        &[],
    ),
    spec(CommandId::Interval1Hour, "Interval: 1 hour", "1h 60m", &[]),
    spec(CommandId::Interval1Day, "Interval: 1 day", "1d daily", &[]),
    spec(CommandId::NewWorkspace, "New workspace", "tab", &["ctrl-t"]),
    spec(
        CommandId::SelectNextWorkspace,
        "Next workspace",
        "tab switch",
        &["ctrl-tab"],
    ),
    spec(
        CommandId::SelectPreviousWorkspace,
        "Previous workspace",
        "tab switch",
        &["ctrl-shift-tab"],
    ),
    spec(
        CommandId::MoveWorkspaceLeft,
        "Move workspace left",
        "tab reorder",
        &["ctrl-shift-pageup"],
    ),
    spec(
        CommandId::MoveWorkspaceRight,
        "Move workspace right",
        "tab reorder",
        &["ctrl-shift-pagedown"],
    ),
    spec(
        CommandId::CloseWorkspace,
        "Close workspace",
        "tab",
        &["ctrl-w"],
    ),
    spec(
        CommandId::SplitHorizontal,
        "Split pane horizontally",
        "layout",
        &["ctrl-alt-h"],
    ),
    spec(
        CommandId::SplitVertical,
        "Split pane vertically",
        "layout",
        &["ctrl-alt-v"],
    ),
    spec(
        CommandId::ClosePane,
        "Close pane",
        "layout",
        &["ctrl-shift-w"],
    ),
    spec(
        CommandId::ToggleOneClickTrading,
        "Trading: Toggle one-click trading",
        "confirm confirmation hotkeys",
        &[],
    ),
    spec(
        CommandId::BuyMarket,
        "Trading: Buy market",
        "order",
        &["ctrl-b"],
    ),
    spec(
        CommandId::SellMarket,
        "Trading: Sell market",
        "order",
        &["ctrl-s"],
    ),
    spec(
        CommandId::CancelAll,
        "Trading: Cancel all",
        "orders",
        &["ctrl-shift-x"],
    ),
    spec(
        CommandId::FlattenAccount,
        "Trading: Flatten account",
        "close positions",
        &["ctrl-shift-f"],
    ),
    spec(
        CommandId::KillSwitch,
        "Trading: Kill switch",
        "cancel flatten lock",
        &["ctrl-shift-k"],
    ),
];

/// Chords Windows owns. An application binding would never fire or would break a system habit.
/// Any chord with the Windows key is rejected as well.
const SYSTEM_CHORDS: &[&str] = &[
    "alt-tab",
    "alt-shift-tab",
    "alt-escape",
    "alt-space",
    "ctrl-escape",
    "ctrl-shift-escape",
    "ctrl-alt-delete",
];

fn same_chord(left: &Keystroke, right: &Keystroke) -> bool {
    left.key == right.key && left.modifiers == right.modifiers
}

/// Checks every chord in `commands`: it parses as a GPUI keystroke, no two bindings share it, it
/// is not a system chord, and it does not shadow a key the chart consumes. `chart_keys` is the
/// key contract Aeris Charts publishes.
///
/// # Errors
///
/// Returns the first chord that fails, naming the conflict.
pub fn validate_chords(commands: &[CommandSpec], chart_keys: &[Keystroke]) -> Result<(), String> {
    let system = SYSTEM_CHORDS
        .iter()
        .map(|chord| Keystroke::parse(chord).map_err(|error| format!("{chord}: {error}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut bound: Vec<(Keystroke, &CommandSpec)> = Vec::new();
    for spec in commands {
        for chord in spec.chords {
            let keystroke = Keystroke::parse(chord)
                .map_err(|error| format!("{} chord {chord}: {error}", spec.title))?;
            if let Some((_, owner)) = bound
                .iter()
                .find(|(other, _)| same_chord(other, &keystroke))
            {
                return Err(format!(
                    "{chord} is bound to both {} and {}",
                    owner.title, spec.title
                ));
            }
            if keystroke.modifiers.platform || system.iter().any(|s| same_chord(s, &keystroke)) {
                return Err(format!("{chord} ({}) is a system shortcut", spec.title));
            }
            if chart_keys.iter().any(|key| same_chord(key, &keystroke)) {
                return Err(format!("{chord} ({}) shadows a chart key", spec.title));
            }
            bound.push((keystroke, spec));
        }
    }
    Ok(())
}

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
    fn chord_validation_rejects_duplicates_system_keys_and_chart_keys() {
        assert_eq!(validate_chords(COMMANDS, &[]), Ok(()));
        let buy = command(CommandId::BuyMarket);
        let sell = CommandSpec {
            chords: &["ctrl-b"],
            ..*command(CommandId::SellMarket)
        };
        let error = validate_chords(&[*buy, sell], &[]).expect_err("duplicate");
        assert!(error.contains("ctrl-b is bound to both"), "{error}");
        let alt_tab = CommandSpec {
            chords: &["alt-tab"],
            ..*buy
        };
        assert!(validate_chords(&[alt_tab], &[]).is_err());
        let windows_key = CommandSpec {
            chords: &["cmd-b"],
            ..*buy
        };
        assert!(validate_chords(&[windows_key], &[]).is_err());
        let malformed = CommandSpec {
            chords: &["meta-b"],
            ..*buy
        };
        assert!(validate_chords(&[malformed], &[]).is_err());
        let chart_key = Keystroke::parse("ctrl-b").expect("chart key");
        let error = validate_chords(&[*buy], &[chart_key]).expect_err("shadowed");
        assert!(error.contains("shadows a chart key"), "{error}");
    }

    #[test]
    fn shortcut_labels_read_as_written_in_menus() {
        assert_eq!(
            command(CommandId::KillSwitch).shortcut_label().as_deref(),
            Some("Ctrl+Shift+K")
        );
        assert_eq!(chord_label("ctrl-shift-pageup"), "Ctrl+Shift+Page Up");
        assert_eq!(chord_keys("alt-enter"), ["Alt", "Enter"]);
        assert_eq!(chord_label("f11"), "F11");
        assert_eq!(command(CommandId::ToggleContext).shortcut_label(), None);
    }

    #[test]
    fn shortcut_sections_list_every_bound_command_once_trading_first() {
        let sections = shortcut_sections();
        let (group, trading) = &sections[0];
        assert_eq!(*group, CommandGroup::Trading);
        assert_eq!(
            trading
                .iter()
                .map(|spec| (spec.short_title(), spec.shortcut_label()))
                .collect::<Vec<_>>(),
            [
                ("Buy market", Some("Ctrl+B".to_string())),
                ("Sell market", Some("Ctrl+S".to_string())),
                ("Cancel all", Some("Ctrl+Shift+X".to_string())),
                ("Flatten account", Some("Ctrl+Shift+F".to_string())),
                ("Kill switch", Some("Ctrl+Shift+K".to_string())),
            ]
        );
        let listed = sections
            .iter()
            .flat_map(|(_, commands)| commands.iter().map(|spec| spec.id))
            .collect::<Vec<_>>();
        let bound = COMMANDS
            .iter()
            .filter(|spec| !spec.chords.is_empty())
            .count();
        assert_eq!(listed.len(), bound);
        assert!(
            sections
                .iter()
                .all(|(group, commands)| commands.iter().all(|spec| spec.id.group() == *group))
        );
        assert_eq!(
            command(CommandId::NewWorkspace).short_title(),
            "New workspace"
        );
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
