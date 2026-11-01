//! Product command metadata shared by menus, keyboard bindings, and the command palette.
//!
//! [`COMMANDS`] and [`WORKSPACE_SHORTCUTS`] are the only sources of desktop shortcuts. Keys the
//! chart consumes belong to Aeris Charts; [`validate_chords`] keeps every shortcut here off them.

use aeris_localization::{Text, shell, tr};
use gpui::{Keystroke, Modifiers, MouseButton};

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
    ToggleOrderEntry,
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
    SaveLayout,
    ToggleLayoutAutosave,
    CopyLayout,
    OpenRecentLayout,
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
    pub const ALL: [Self; 40] = [
        Self::OpenPalette,
        Self::ToggleFullscreen,
        Self::MinimizeWindow,
        Self::ZoomWindow,
        Self::CloseWindow,
        Self::ToggleContext,
        Self::ToggleOrderBook,
        Self::ToggleOrderEntry,
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
        Self::SaveLayout,
        Self::ToggleLayoutAutosave,
        Self::CopyLayout,
        Self::OpenRecentLayout,
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
            | Self::ToggleOrderEntry
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
            | Self::CloseWorkspace
            | Self::SaveLayout
            | Self::ToggleLayoutAutosave
            | Self::CopyLayout
            | Self::OpenRecentLayout => CommandGroup::Workspaces,
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
    pub const fn label(self) -> Text {
        match self {
            Self::Trading => shell::GROUP_TRADING,
            Self::General => shell::GROUP_GENERAL,
            Self::Workspaces => shell::GROUP_WORKSPACES,
            Self::Panes => shell::GROUP_PANES,
            Self::Panels => shell::GROUP_PANELS,
            Self::Chart => shell::GROUP_CHART,
        }
    }
}

/// One line of the shortcuts list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutRow {
    pub title: Text,
    /// Alternatives, the displayed shortcut first.
    pub triggers: Vec<ShortcutTrigger>,
}

/// Every shortcut, grouped under its heading: the bound commands in registry order, then the
/// workspace shortcuts. Groups without a shortcut are left out.
#[must_use]
pub fn shortcut_sections() -> Vec<(CommandGroup, Vec<ShortcutRow>)> {
    CommandGroup::ALL
        .into_iter()
        .filter_map(|group| {
            let commands = COMMANDS
                .iter()
                .filter(|spec| spec.id.group() == group && !spec.chords.is_empty())
                .map(|spec| ShortcutRow {
                    title: spec.short_title,
                    triggers: spec
                        .chords
                        .iter()
                        .copied()
                        .map(ShortcutTrigger::Key)
                        .collect(),
                });
            let workspace = WORKSPACE_SHORTCUTS
                .iter()
                .filter(|spec| spec.group == group)
                .map(|spec| ShortcutRow {
                    title: spec.title,
                    triggers: vec![spec.trigger],
                });
            let rows = commands.chain(workspace).collect::<Vec<_>>();
            (!rows.is_empty()).then_some((group, rows))
        })
        .collect()
}

/// Shortcuts the workspace matches itself instead of binding them in the GPUI keymap. They act
/// on the focused chart or on the pane under the pointer, so they must give way to text fields
/// and chart drawing, which a window-wide binding cannot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceShortcut {
    MaximizePane,
    ToggleChartFullscreen,
    NextWatchlistSymbol,
    PreviousWatchlistSymbol,
}

/// What the user presses for a shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutTrigger {
    /// A GPUI keystroke such as `shift-f`, matched on its exact modifiers.
    Key(&'static str),
    /// A primary-button press with Alt held.
    AltClick,
}

impl ShortcutTrigger {
    /// The keys as people write them, for keycaps: `shift-f` is `Shift`, `F`.
    #[must_use]
    pub fn keys(self) -> Vec<String> {
        match self {
            Self::Key(chord) => chord_keys(chord),
            Self::AltClick => vec!["Alt".to_string(), "Click".to_string()],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceShortcutSpec {
    pub shortcut: WorkspaceShortcut,
    pub group: CommandGroup,
    pub title: Text,
    pub trigger: ShortcutTrigger,
}

pub const WORKSPACE_SHORTCUTS: &[WorkspaceShortcutSpec] = &[
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::MaximizePane,
        group: CommandGroup::Panes,
        title: shell::WS_MAXIMIZE_PANE,
        trigger: ShortcutTrigger::AltClick,
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::ToggleChartFullscreen,
        group: CommandGroup::Chart,
        title: shell::WS_CHART_FULLSCREEN,
        trigger: ShortcutTrigger::Key("shift-f"),
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::NextWatchlistSymbol,
        group: CommandGroup::Chart,
        title: shell::WS_NEXT_WATCHLIST_SYMBOL,
        trigger: ShortcutTrigger::Key("space"),
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::PreviousWatchlistSymbol,
        group: CommandGroup::Chart,
        title: shell::WS_PREVIOUS_WATCHLIST_SYMBOL,
        trigger: ShortcutTrigger::Key("shift-space"),
    },
];

/// The workspace shortcut a key press triggers, if any.
#[must_use]
pub fn workspace_key_shortcut(keystroke: &Keystroke) -> Option<WorkspaceShortcut> {
    WORKSPACE_SHORTCUTS
        .iter()
        .find_map(|spec| match spec.trigger {
            ShortcutTrigger::Key(chord) => Keystroke::parse(chord)
                .is_ok_and(|bound| same_chord(&bound, keystroke))
                .then_some(spec.shortcut),
            ShortcutTrigger::AltClick => None,
        })
}

/// The workspace shortcut a mouse press triggers, if any.
#[must_use]
pub fn workspace_click_shortcut(
    button: MouseButton,
    modifiers: Modifiers,
) -> Option<WorkspaceShortcut> {
    if button != MouseButton::Left || !modifiers.alt {
        return None;
    }
    WORKSPACE_SHORTCUTS
        .iter()
        .find(|spec| spec.trigger == ShortcutTrigger::AltClick)
        .map(|spec| spec.shortcut)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: Text,
    /// The title without its group prefix, for lists that already show the group as a heading
    /// (`Buy market` under Trading). A command whose title carries no prefix repeats it.
    pub short_title: Text,
    pub keywords: Text,
    /// GPUI keystrokes bound to the command, the displayed shortcut first.
    pub chords: &'static [&'static str],
}

impl CommandSpec {
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
    title: Text,
    keywords: Text,
    chords: &'static [&'static str],
) -> CommandSpec {
    CommandSpec {
        id,
        title,
        short_title: title,
        keywords,
        chords,
    }
}

/// A command whose title carries its group as a prefix (`Chart: Candles` under Chart); the
/// short title is its own catalog text, never a slice of the translated title.
const fn prefixed_spec(
    id: CommandId,
    title: Text,
    short_title: Text,
    keywords: Text,
    chords: &'static [&'static str],
) -> CommandSpec {
    CommandSpec {
        id,
        title,
        short_title,
        keywords,
        chords,
    }
}

pub const COMMANDS: &[CommandSpec] = &[
    spec(
        CommandId::OpenPalette,
        shell::CMD_OPEN_PALETTE,
        shell::CMD_OPEN_PALETTE_KEYWORDS,
        &["ctrl-k"],
    ),
    spec(
        CommandId::ToggleFullscreen,
        shell::CMD_TOGGLE_FULLSCREEN,
        shell::CMD_TOGGLE_FULLSCREEN_KEYWORDS,
        &["f11", "alt-enter"],
    ),
    spec(
        CommandId::MinimizeWindow,
        shell::CMD_MINIMIZE_WINDOW,
        shell::CMD_MINIMIZE_WINDOW_KEYWORDS,
        &["alt-f9"],
    ),
    spec(
        CommandId::ZoomWindow,
        shell::CMD_ZOOM_WINDOW,
        shell::CMD_ZOOM_WINDOW_KEYWORDS,
        &["alt-f10"],
    ),
    spec(
        CommandId::CloseWindow,
        shell::CMD_CLOSE_WINDOW,
        shell::CMD_CLOSE_WINDOW_KEYWORDS,
        &["alt-f4"],
    ),
    spec(
        CommandId::ToggleContext,
        shell::CMD_TOGGLE_CONTEXT,
        shell::CMD_TOGGLE_CONTEXT_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::ToggleOrderBook,
        shell::CMD_TOGGLE_ORDER_BOOK,
        shell::CMD_TOGGLE_ORDER_BOOK_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::ToggleOrderEntry,
        shell::CMD_TOGGLE_ORDER_ENTRY,
        shell::CMD_TOGGLE_ORDER_ENTRY_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::ToggleTimeSales,
        shell::CMD_TOGGLE_TIME_SALES,
        shell::CMD_TOGGLE_TIME_SALES_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::ToggleWatchlist,
        shell::CMD_TOGGLE_WATCHLIST,
        shell::CMD_TOGGLE_WATCHLIST_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::ChartCandles,
        shell::CMD_CHART_CANDLES,
        shell::CMD_CHART_CANDLES_SHORT,
        shell::CMD_CHART_CANDLES_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::ChartBars,
        shell::CMD_CHART_BARS,
        shell::CMD_CHART_BARS_SHORT,
        shell::CMD_CHART_BARS_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::ChartLine,
        shell::CMD_CHART_LINE,
        shell::CMD_CHART_LINE_SHORT,
        shell::CMD_CHART_LINE_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::ChartArea,
        shell::CMD_CHART_AREA,
        shell::CMD_CHART_AREA_SHORT,
        shell::CMD_CHART_AREA_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::ChartBaseline,
        shell::CMD_CHART_BASELINE,
        shell::CMD_CHART_BASELINE_SHORT,
        shell::CMD_CHART_BASELINE_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::ChartFootprint,
        shell::CMD_CHART_FOOTPRINT,
        shell::CMD_CHART_FOOTPRINT_SHORT,
        shell::CMD_CHART_FOOTPRINT_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::Interval1Minute,
        shell::CMD_INTERVAL_1_MINUTE,
        shell::CMD_INTERVAL_1_MINUTE_SHORT,
        shell::CMD_INTERVAL_1_MINUTE_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::Interval5Minutes,
        shell::CMD_INTERVAL_5_MINUTES,
        shell::CMD_INTERVAL_5_MINUTES_SHORT,
        shell::CMD_INTERVAL_5_MINUTES_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::Interval15Minutes,
        shell::CMD_INTERVAL_15_MINUTES,
        shell::CMD_INTERVAL_15_MINUTES_SHORT,
        shell::CMD_INTERVAL_15_MINUTES_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::Interval1Hour,
        shell::CMD_INTERVAL_1_HOUR,
        shell::CMD_INTERVAL_1_HOUR_SHORT,
        shell::CMD_INTERVAL_1_HOUR_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::Interval1Day,
        shell::CMD_INTERVAL_1_DAY,
        shell::CMD_INTERVAL_1_DAY_SHORT,
        shell::CMD_INTERVAL_1_DAY_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::NewWorkspace,
        shell::CMD_NEW_WORKSPACE,
        shell::CMD_NEW_WORKSPACE_KEYWORDS,
        &["ctrl-t"],
    ),
    spec(
        CommandId::SelectNextWorkspace,
        shell::CMD_NEXT_WORKSPACE,
        shell::CMD_NEXT_WORKSPACE_KEYWORDS,
        &["ctrl-tab"],
    ),
    spec(
        CommandId::SelectPreviousWorkspace,
        shell::CMD_PREVIOUS_WORKSPACE,
        shell::CMD_PREVIOUS_WORKSPACE_KEYWORDS,
        &["ctrl-shift-tab"],
    ),
    spec(
        CommandId::MoveWorkspaceLeft,
        shell::CMD_MOVE_WORKSPACE_LEFT,
        shell::CMD_MOVE_WORKSPACE_LEFT_KEYWORDS,
        &["ctrl-shift-pageup"],
    ),
    spec(
        CommandId::MoveWorkspaceRight,
        shell::CMD_MOVE_WORKSPACE_RIGHT,
        shell::CMD_MOVE_WORKSPACE_RIGHT_KEYWORDS,
        &["ctrl-shift-pagedown"],
    ),
    spec(
        CommandId::CloseWorkspace,
        shell::CMD_CLOSE_WORKSPACE,
        shell::CMD_CLOSE_WORKSPACE_KEYWORDS,
        &["ctrl-w"],
    ),
    spec(
        CommandId::SaveLayout,
        shell::CMD_SAVE_LAYOUT,
        shell::CMD_SAVE_LAYOUT_KEYWORDS,
        &["ctrl-s"],
    ),
    spec(
        CommandId::ToggleLayoutAutosave,
        shell::CMD_LAYOUT_AUTOSAVE,
        shell::CMD_LAYOUT_AUTOSAVE_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::CopyLayout,
        shell::CMD_COPY_LAYOUT,
        shell::CMD_COPY_LAYOUT_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::OpenRecentLayout,
        shell::CMD_OPEN_RECENT_LAYOUT,
        shell::CMD_OPEN_RECENT_LAYOUT_KEYWORDS,
        &[],
    ),
    spec(
        CommandId::SplitHorizontal,
        shell::CMD_SPLIT_HORIZONTAL,
        shell::CMD_SPLIT_HORIZONTAL_KEYWORDS,
        &["ctrl-alt-h"],
    ),
    spec(
        CommandId::SplitVertical,
        shell::CMD_SPLIT_VERTICAL,
        shell::CMD_SPLIT_VERTICAL_KEYWORDS,
        &["ctrl-alt-v"],
    ),
    spec(
        CommandId::ClosePane,
        shell::CMD_CLOSE_PANE,
        shell::CMD_CLOSE_PANE_KEYWORDS,
        &["ctrl-shift-w"],
    ),
    prefixed_spec(
        CommandId::ToggleOneClickTrading,
        shell::CMD_TRADING_ONE_CLICK,
        shell::CMD_TRADING_ONE_CLICK_SHORT,
        shell::CMD_TRADING_ONE_CLICK_KEYWORDS,
        &[],
    ),
    prefixed_spec(
        CommandId::BuyMarket,
        shell::CMD_BUY_MARKET,
        shell::CMD_BUY_MARKET_SHORT,
        shell::CMD_BUY_MARKET_KEYWORDS,
        &["ctrl-b"],
    ),
    prefixed_spec(
        CommandId::SellMarket,
        shell::CMD_SELL_MARKET,
        shell::CMD_SELL_MARKET_SHORT,
        shell::CMD_SELL_MARKET_KEYWORDS,
        &["ctrl-shift-s"],
    ),
    prefixed_spec(
        CommandId::CancelAll,
        shell::CMD_CANCEL_ALL,
        shell::CMD_CANCEL_ALL_SHORT,
        shell::CMD_CANCEL_ALL_KEYWORDS,
        &["ctrl-shift-x"],
    ),
    prefixed_spec(
        CommandId::FlattenAccount,
        shell::CMD_FLATTEN_ACCOUNT,
        shell::CMD_FLATTEN_ACCOUNT_SHORT,
        shell::CMD_FLATTEN_ACCOUNT_KEYWORDS,
        &["ctrl-shift-f"],
    ),
    prefixed_spec(
        CommandId::KillSwitch,
        shell::CMD_KILL_SWITCH,
        shell::CMD_KILL_SWITCH_SHORT,
        shell::CMD_KILL_SWITCH_KEYWORDS,
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

/// Checks every key chord in `commands` and `workspace`: it parses as a GPUI keystroke, no two
/// shortcuts share it, it is not a system chord, and it does not shadow a key the chart
/// consumes. `chart_keys` is the key contract Aeris Charts publishes.
///
/// # Errors
///
/// Returns the first chord that fails, naming the conflict.
pub fn validate_chords(
    commands: &[CommandSpec],
    workspace: &[WorkspaceShortcutSpec],
    chart_keys: &[Keystroke],
) -> Result<(), String> {
    let system = SYSTEM_CHORDS
        .iter()
        .map(|chord| Keystroke::parse(chord).map_err(|error| format!("{chord}: {error}")))
        .collect::<Result<Vec<_>, _>>()?;
    let command_chords = commands
        .iter()
        .flat_map(|spec| spec.chords.iter().map(|chord| (spec.title, *chord)));
    let workspace_chords = workspace.iter().filter_map(|spec| match spec.trigger {
        ShortcutTrigger::Key(chord) => Some((spec.title, chord)),
        ShortcutTrigger::AltClick => None,
    });
    let mut bound: Vec<(Keystroke, Text)> = Vec::new();
    for (title, chord) in command_chords.chain(workspace_chords) {
        let keystroke =
            Keystroke::parse(chord).map_err(|error| format!("{title} chord {chord}: {error}"))?;
        if let Some((_, owner)) = bound
            .iter()
            .find(|(other, _)| same_chord(other, &keystroke))
        {
            return Err(format!("{chord} is bound to both {owner} and {title}"));
        }
        if keystroke.modifiers.platform || system.iter().any(|s| same_chord(s, &keystroke)) {
            return Err(format!("{chord} ({title}) is a system shortcut"));
        }
        if chart_keys.iter().any(|key| same_chord(key, &keystroke)) {
            return Err(format!("{chord} ({title}) shadows a chart key"));
        }
        bound.push((keystroke, title));
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
    let query = query.trim().to_lowercase();
    COMMANDS
        .iter()
        .filter(|spec| {
            query.is_empty()
                || tr!(spec.title).to_lowercase().contains(&query)
                || tr!(spec.keywords)
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
        assert_eq!(
            tr!(command(CommandId::ToggleContext).title),
            "Market context"
        );
    }

    #[test]
    fn chord_validation_rejects_duplicates_system_keys_and_chart_keys() {
        assert_eq!(validate_chords(COMMANDS, WORKSPACE_SHORTCUTS, &[]), Ok(()));
        let buy = command(CommandId::BuyMarket);
        let sell = CommandSpec {
            chords: &["ctrl-b"],
            ..*command(CommandId::SellMarket)
        };
        let error = validate_chords(&[*buy, sell], &[], &[]).expect_err("duplicate");
        assert!(error.contains("ctrl-b is bound to both"), "{error}");
        let alt_tab = CommandSpec {
            chords: &["alt-tab"],
            ..*buy
        };
        assert!(validate_chords(&[alt_tab], &[], &[]).is_err());
        let windows_key = CommandSpec {
            chords: &["cmd-b"],
            ..*buy
        };
        assert!(validate_chords(&[windows_key], &[], &[]).is_err());
        let malformed = CommandSpec {
            chords: &["meta-b"],
            ..*buy
        };
        assert!(validate_chords(&[malformed], &[], &[]).is_err());
        let chart_key = Keystroke::parse("ctrl-b").expect("chart key");
        let error = validate_chords(&[*buy], &[], &[chart_key]).expect_err("shadowed");
        assert!(error.contains("shadows a chart key"), "{error}");

        let fullscreen = WorkspaceShortcutSpec {
            trigger: ShortcutTrigger::Key("ctrl-b"),
            ..WORKSPACE_SHORTCUTS[1]
        };
        let error = validate_chords(&[*buy], &[fullscreen], &[]).expect_err("duplicate");
        assert!(error.contains("ctrl-b is bound to both"), "{error}");
        let space = Keystroke::parse("space").expect("chart key");
        let error = validate_chords(&[], WORKSPACE_SHORTCUTS, &[space]).expect_err("shadowed");
        assert!(error.contains("Next watchlist symbol"), "{error}");
    }

    #[test]
    fn workspace_shortcuts_match_exact_keys_and_alt_click() {
        let key = |chord| workspace_key_shortcut(&Keystroke::parse(chord).expect("keystroke"));
        assert_eq!(
            key("shift-f"),
            Some(WorkspaceShortcut::ToggleChartFullscreen)
        );
        assert_eq!(key("space"), Some(WorkspaceShortcut::NextWatchlistSymbol));
        assert_eq!(
            key("shift-space"),
            Some(WorkspaceShortcut::PreviousWatchlistSymbol)
        );
        assert_eq!(key("f"), None, "a plain F still starts symbol search");
        assert_eq!(key("ctrl-space"), None);
        assert_eq!(key("ctrl-shift-f"), None, "Ctrl+Shift+F stays flatten");

        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(
            workspace_click_shortcut(MouseButton::Left, alt),
            Some(WorkspaceShortcut::MaximizePane)
        );
        assert_eq!(
            workspace_click_shortcut(MouseButton::Left, Modifiers::default()),
            None
        );
        assert_eq!(workspace_click_shortcut(MouseButton::Right, alt), None);
        assert_eq!(ShortcutTrigger::AltClick.keys(), ["Alt", "Click"]);
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
    fn shortcut_sections_list_every_shortcut_once_trading_first() {
        let sections = shortcut_sections();
        let labels = |rows: &[ShortcutRow]| {
            rows.iter()
                .map(|row| {
                    let keys = row
                        .triggers
                        .iter()
                        .map(|trigger| trigger.keys().join("+"))
                        .collect::<Vec<_>>();
                    (tr!(row.title), keys.join(" or "))
                })
                .collect::<Vec<_>>()
        };
        let (group, trading) = &sections[0];
        assert_eq!(*group, CommandGroup::Trading);
        assert_eq!(
            labels(trading),
            [
                ("Buy market", "Ctrl+B".to_string()),
                ("Sell market", "Ctrl+Shift+S".to_string()),
                ("Cancel all", "Ctrl+Shift+X".to_string()),
                ("Flatten account", "Ctrl+Shift+F".to_string()),
                ("Kill switch", "Ctrl+Shift+K".to_string()),
            ]
        );
        let section = |wanted| {
            sections
                .iter()
                .find(|(group, _)| *group == wanted)
                .map(|(_, rows)| labels(rows))
                .expect("section")
        };
        assert!(
            section(CommandGroup::Workspaces).contains(&("Save layout", "Ctrl+S".to_string())),
            "Ctrl+S saves the layout"
        );
        assert_eq!(
            section(CommandGroup::Panes).last(),
            Some(&("Maximize or restore pane", "Alt+Click".to_string()))
        );
        assert_eq!(
            section(CommandGroup::Chart),
            [
                ("Chart fullscreen", "Shift+F".to_string()),
                ("Next watchlist symbol", "Space".to_string()),
                ("Previous watchlist symbol", "Shift+Space".to_string()),
            ]
        );
        let listed = sections.iter().map(|(_, rows)| rows.len()).sum::<usize>();
        let bound = COMMANDS
            .iter()
            .filter(|spec| !spec.chords.is_empty())
            .count();
        assert_eq!(listed, bound + WORKSPACE_SHORTCUTS.len());
        assert_eq!(
            tr!(command(CommandId::NewWorkspace).short_title),
            "New workspace"
        );
        assert_eq!(
            tr!(command(CommandId::BuyMarket).short_title),
            "Buy market",
            "the short title is its own text, not a slice of the translated title"
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
