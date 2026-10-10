//! Product command metadata shared by menus, keyboard bindings, and the command palette.
//!
//! [`COMMANDS`] and [`WORKSPACE_SHORTCUTS`] are the only sources of desktop shortcuts. Keys the
//! chart consumes belong to Aeris Charts; [`validate_chords`] keeps every shortcut here off them.

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

/// One line of the shortcuts list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutRow {
    pub title: &'static str,
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
                    title: spec.short_title(),
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
    pub title: &'static str,
    pub trigger: ShortcutTrigger,
}

pub const WORKSPACE_SHORTCUTS: &[WorkspaceShortcutSpec] = &[
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::MaximizePane,
        group: CommandGroup::Panes,
        title: "Maximize or restore pane",
        trigger: ShortcutTrigger::AltClick,
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::ToggleChartFullscreen,
        group: CommandGroup::Chart,
        title: "Chart fullscreen",
        trigger: ShortcutTrigger::Key("shift-f"),
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::NextWatchlistSymbol,
        group: CommandGroup::Chart,
        title: "Next watchlist symbol",
        trigger: ShortcutTrigger::Key("space"),
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::PreviousWatchlistSymbol,
        group: CommandGroup::Chart,
        title: "Previous watchlist symbol",
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
    let mut bound: Vec<(Keystroke, &str)> = Vec::new();
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
                    (row.title, keys.join(" or "))
                })
                .collect::<Vec<_>>()
        };
        let (group, trading) = &sections[0];
        assert_eq!(*group, CommandGroup::Trading);
        assert_eq!(
            labels(trading),
            [
                ("Buy market", "Ctrl+B".to_string()),
                ("Sell market", "Ctrl+S".to_string()),
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
