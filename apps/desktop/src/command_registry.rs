//! Product command metadata shared by menus, keyboard bindings, and the command palette.
//!
//! [`COMMANDS`] and [`WORKSPACE_SHORTCUTS`] hold the default desktop shortcuts and
//! [`ShortcutOverrides`] the user's changes to them, which the chart chrome saves. Keys the
//! chart consumes belong to Aeris Charts; [`validate_chords`] and every override edit keep the
//! shortcuts here off them.

use aeris_contracts::WorkspaceShortcutOverrideState;
use aeris_localization::{Text, shell, tr};
use gpui::{Keystroke, Modifiers, MouseButton};
use std::fmt;

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

    /// The number of commands; [`ShortcutOverrides`] keeps one slot for each.
    pub const COUNT: usize = Self::ALL.len();

    /// The command's position in [`Self::ALL`], which lists them in declaration order.
    const fn index(self) -> usize {
        self as usize
    }

    /// Whether a shortcut can run the command: it has a GPUI key action. The others run only
    /// from the command palette and never take a shortcut.
    #[must_use]
    pub const fn accepts_shortcut(self) -> bool {
        matches!(
            self,
            Self::OpenPalette
                | Self::ToggleFullscreen
                | Self::MinimizeWindow
                | Self::ZoomWindow
                | Self::CloseWindow
                | Self::NewWorkspace
                | Self::SelectNextWorkspace
                | Self::SelectPreviousWorkspace
                | Self::MoveWorkspaceLeft
                | Self::MoveWorkspaceRight
                | Self::CloseWorkspace
                | Self::SaveLayout
                | Self::SplitHorizontal
                | Self::SplitVertical
                | Self::ClosePane
                | Self::ToggleOneClickTrading
                | Self::BuyMarket
                | Self::SellMarket
                | Self::CancelAll
                | Self::FlattenAccount
                | Self::KillSwitch
        )
    }

    /// The identifier a saved shortcut override names the command by. Saved documents keep
    /// it, so a released identifier never changes.
    #[must_use]
    pub const fn persisted_id(self) -> &'static str {
        match self {
            Self::OpenPalette => "open_palette",
            Self::ToggleFullscreen => "toggle_fullscreen",
            Self::MinimizeWindow => "minimize_window",
            Self::ZoomWindow => "zoom_window",
            Self::CloseWindow => "close_window",
            Self::ToggleContext => "toggle_context",
            Self::ToggleOrderBook => "toggle_order_book",
            Self::ToggleOrderEntry => "toggle_order_entry",
            Self::ToggleTimeSales => "toggle_time_sales",
            Self::ToggleWatchlist => "toggle_watchlist",
            Self::ChartCandles => "chart_candles",
            Self::ChartBars => "chart_bars",
            Self::ChartLine => "chart_line",
            Self::ChartArea => "chart_area",
            Self::ChartBaseline => "chart_baseline",
            Self::ChartFootprint => "chart_footprint",
            Self::Interval1Minute => "interval_1m",
            Self::Interval5Minutes => "interval_5m",
            Self::Interval15Minutes => "interval_15m",
            Self::Interval1Hour => "interval_1h",
            Self::Interval1Day => "interval_1d",
            Self::NewWorkspace => "new_workspace",
            Self::SelectNextWorkspace => "next_workspace",
            Self::SelectPreviousWorkspace => "previous_workspace",
            Self::MoveWorkspaceLeft => "move_workspace_left",
            Self::MoveWorkspaceRight => "move_workspace_right",
            Self::CloseWorkspace => "close_workspace",
            Self::SaveLayout => "save_layout",
            Self::ToggleLayoutAutosave => "toggle_layout_autosave",
            Self::CopyLayout => "copy_layout",
            Self::OpenRecentLayout => "open_recent_layout",
            Self::SplitHorizontal => "split_horizontal",
            Self::SplitVertical => "split_vertical",
            Self::ClosePane => "close_pane",
            Self::ToggleOneClickTrading => "toggle_one_click_trading",
            Self::BuyMarket => "buy_market",
            Self::SellMarket => "sell_market",
            Self::CancelAll => "cancel_all",
            Self::FlattenAccount => "flatten_account",
            Self::KillSwitch => "kill_switch",
        }
    }

    /// The command a saved identifier names, if this version knows it.
    #[must_use]
    pub fn from_persisted_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.persisted_id() == value)
    }

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
    /// The command a user can rebind from this line; workspace shortcuts have none.
    pub command: Option<CommandId>,
    /// Alternatives, the displayed shortcut first. Empty while the command has no shortcut.
    pub triggers: Vec<ShortcutTrigger>,
    /// The user changed the command's keys from their defaults.
    pub customized: bool,
}

/// Every shortcut with `overrides` applied, grouped under its heading: each command a shortcut
/// can run, bound or not, in registry order, then the workspace shortcuts. Groups without a
/// shortcut are left out.
#[must_use]
pub fn shortcut_sections(overrides: &ShortcutOverrides) -> Vec<(CommandGroup, Vec<ShortcutRow>)> {
    CommandGroup::ALL
        .into_iter()
        .filter_map(|group| {
            let commands = COMMANDS
                .iter()
                .filter(|spec| spec.id.group() == group && spec.id.accepts_shortcut())
                .map(|spec| ShortcutRow {
                    title: spec.short_title,
                    command: Some(spec.id),
                    triggers: overrides
                        .chords(spec.id)
                        .into_iter()
                        .map(ShortcutTrigger::Key)
                        .collect(),
                    customized: overrides.is_customized(spec.id),
                });
            let workspace = WORKSPACE_SHORTCUTS
                .iter()
                .filter(|spec| spec.group == group)
                .map(|spec| ShortcutRow {
                    title: spec.shortcut.title(),
                    command: None,
                    triggers: vec![spec.trigger],
                    customized: false,
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

impl WorkspaceShortcut {
    #[must_use]
    pub const fn title(self) -> Text {
        match self {
            Self::MaximizePane => shell::WS_MAXIMIZE_PANE,
            Self::ToggleChartFullscreen => shell::WS_CHART_FULLSCREEN,
            Self::NextWatchlistSymbol => shell::WS_NEXT_WATCHLIST_SYMBOL,
            Self::PreviousWatchlistSymbol => shell::WS_PREVIOUS_WATCHLIST_SYMBOL,
        }
    }
}

/// What the user presses for a shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutTrigger {
    /// A key chord such as Shift+F, matched on its exact modifiers.
    Key(ShortcutChord),
    /// A primary-button press with Alt held.
    AltClick,
}

impl ShortcutTrigger {
    /// The keys as people write them, for keycaps: Shift+F is `Shift`, `F`.
    #[must_use]
    pub fn keys(self) -> Vec<String> {
        match self {
            Self::Key(chord) => chord.keys(),
            Self::AltClick => vec!["Alt".to_string(), "Click".to_string()],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceShortcutSpec {
    pub shortcut: WorkspaceShortcut,
    pub group: CommandGroup,
    pub trigger: ShortcutTrigger,
}

pub const WORKSPACE_SHORTCUTS: &[WorkspaceShortcutSpec] = &[
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::MaximizePane,
        group: CommandGroup::Panes,
        trigger: ShortcutTrigger::AltClick,
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::ToggleChartFullscreen,
        group: CommandGroup::Chart,
        trigger: ShortcutTrigger::Key(ShortcutChord::named("f").shift()),
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::NextWatchlistSymbol,
        group: CommandGroup::Chart,
        trigger: ShortcutTrigger::Key(ShortcutChord::named("space")),
    },
    WorkspaceShortcutSpec {
        shortcut: WorkspaceShortcut::PreviousWatchlistSymbol,
        group: CommandGroup::Chart,
        trigger: ShortcutTrigger::Key(ShortcutChord::named("space").shift()),
    },
];

/// The workspace shortcut a key press triggers, if any.
#[must_use]
pub fn workspace_key_shortcut(keystroke: &Keystroke) -> Option<WorkspaceShortcut> {
    WORKSPACE_SHORTCUTS
        .iter()
        .find_map(|spec| match spec.trigger {
            ShortcutTrigger::Key(chord) => chord.matches(keystroke).then_some(spec.shortcut),
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
    /// The default key chords bound to the command, the displayed shortcut first.
    pub chords: &'static [ShortcutChord],
}

/// The key names GPUI reports for a modifier pressed on its own; no shortcut ends on one.
const MODIFIER_KEY_NAMES: &[&str] = &["shift", "control", "alt", "platform", "function"];

/// Printable ASCII from `!` to `~`; a one-character key's code is its byte, and its name the
/// character's slice of this text.
const PRINTABLE_KEYS: &str = "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

/// The first byte of [`PRINTABLE_KEYS`].
const FIRST_PRINTABLE_KEY: u8 = b'!';

/// The keys GPUI names instead of writing their character.
const NAMED_KEYS: &[&str] = &[
    "space",
    "tab",
    "enter",
    "escape",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
    "f21",
    "f22",
    "f23",
    "f24",
];

/// The code of the first named key; every code below it is a printable character's byte.
const NAMED_KEY_BASE: u8 = 0x80;

/// One key a shortcut can end on, named as GPUI names keys in its keystrokes: a printable
/// ASCII character other than an uppercase letter, such as `b` or `-`, or a named key such as
/// `f11` or `pageup`. It is one byte, so the chrome preferences that keep every command's
/// override stay small enough to copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShortcutKey(u8);

impl ShortcutKey {
    const fn new(name: &str) -> Option<Self> {
        if let [byte] = name.as_bytes() {
            return if byte.is_ascii_graphic() && !byte.is_ascii_uppercase() {
                Some(Self(*byte))
            } else {
                None
            };
        }
        let mut index = 0;
        let mut code = NAMED_KEY_BASE;
        while index < NAMED_KEYS.len() {
            if same_text(NAMED_KEYS[index], name) {
                return Some(Self(code));
            }
            index += 1;
            code += 1;
        }
        None
    }

    fn name(self) -> &'static str {
        if self.0 < NAMED_KEY_BASE {
            let index = usize::from(self.0.saturating_sub(FIRST_PRINTABLE_KEY));
            PRINTABLE_KEYS.get(index..=index).unwrap_or_default()
        } else {
            NAMED_KEYS
                .get(usize::from(self.0 - NAMED_KEY_BASE))
                .copied()
                .unwrap_or_default()
        }
    }
}

/// `left == right`, which `str` cannot answer in a constant.
const fn same_text(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}
/// A key chord a desktop shortcut is pressed with: any of Ctrl, Alt and Shift with one key.
/// Chords with the Windows or Command key belong to the system, so none holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortcutChord {
    control: bool,
    alt: bool,
    shift: bool,
    key: ShortcutKey,
}

impl ShortcutChord {
    /// The key `name` without modifiers, for the registry tables.
    ///
    /// # Panics
    ///
    /// When `name` is not a lowercase ASCII key name of at most 16 bytes. The tables are
    /// constants, so a bad name fails the build.
    #[must_use]
    pub const fn named(name: &'static str) -> Self {
        match ShortcutKey::new(name) {
            Some(key) => Self {
                control: false,
                alt: false,
                shift: false,
                key,
            },
            None => panic!("shortcut keys are short lowercase ASCII key names"),
        }
    }

    #[must_use]
    pub const fn ctrl(self) -> Self {
        Self {
            control: true,
            ..self
        }
    }

    #[must_use]
    pub const fn alt(self) -> Self {
        Self { alt: true, ..self }
    }

    #[must_use]
    pub const fn shift(self) -> Self {
        Self {
            shift: true,
            ..self
        }
    }

    /// The chord a key press makes, for recording a new shortcut. Fn is not part of a chord:
    /// some keyboards report it with the function keys, and no registry chord holds it.
    ///
    /// # Errors
    ///
    /// [`ShortcutRefusal::System`] for a press with the Windows or Command key, and
    /// [`ShortcutRefusal::Unsupported`] for a modifier alone or a key no shortcut can store.
    pub fn from_keystroke(keystroke: &Keystroke) -> Result<Self, ShortcutRefusal> {
        if keystroke.modifiers.platform {
            return Err(ShortcutRefusal::System);
        }
        if MODIFIER_KEY_NAMES.contains(&keystroke.key.as_str()) {
            return Err(ShortcutRefusal::Unsupported);
        }
        let key = ShortcutKey::new(&keystroke.key).ok_or(ShortcutRefusal::Unsupported)?;
        Ok(Self {
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            shift: keystroke.modifiers.shift,
            key,
        })
    }

    /// Reads a chord as [`Self::source`] writes it, such as `ctrl-shift-b` or `ctrl--`;
    /// anything else, including the Windows or Command key, is `None`.
    #[must_use]
    pub fn parse(source: &str) -> Option<Self> {
        let (mut control, mut alt, mut shift) = (false, false, false);
        let mut rest = source;
        loop {
            let (held, tail) = if let Some(tail) = rest.strip_prefix("ctrl-") {
                (&mut control, tail)
            } else if let Some(tail) = rest.strip_prefix("alt-") {
                (&mut alt, tail)
            } else if let Some(tail) = rest.strip_prefix("shift-") {
                (&mut shift, tail)
            } else {
                break;
            };
            if *held || tail.is_empty() {
                return None;
            }
            *held = true;
            rest = tail;
        }
        if (rest.len() > 1 && rest.contains('-')) || MODIFIER_KEY_NAMES.contains(&rest) {
            return None;
        }
        let key = ShortcutKey::new(rest)?;
        Some(Self {
            control,
            alt,
            shift,
            key,
        })
    }

    /// The chord as GPUI key bindings and saved overrides write it: `ctrl-alt-shift-<key>`.
    #[must_use]
    pub fn source(&self) -> String {
        let mut source = String::new();
        for (held, modifier) in [
            (self.control, "ctrl-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
        ] {
            if held {
                source.push_str(modifier);
            }
        }
        source.push_str(self.key.name());
        source
    }

    /// The keys as people write them, for keycaps: Ctrl+Shift+PageUp is `Ctrl`, `Shift`,
    /// `Page Up`.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        [
            (self.control, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ]
        .into_iter()
        .filter(|&(held, _)| held)
        .map(|(_, modifier)| modifier.to_string())
        .chain([key_label(self.key.name())])
        .collect()
    }

    /// The shortcut written for people, such as `Ctrl+Shift+K`.
    #[must_use]
    pub fn label(&self) -> String {
        self.keys().join("+")
    }

    /// Whether `keystroke` presses exactly this chord.
    #[must_use]
    pub fn matches(&self, keystroke: &Keystroke) -> bool {
        keystroke.key == self.key.name()
            && keystroke.modifiers
                == Modifiers {
                    control: self.control,
                    alt: self.alt,
                    shift: self.shift,
                    platform: false,
                    function: false,
                }
    }

    /// Without Ctrl or Alt a window-wide binding would take the key away from typing; only
    /// the function keys type nothing on their own.
    fn types_text(self) -> bool {
        let function_key = self
            .key
            .name()
            .strip_prefix('f')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=24).contains(&number));
        !(self.control || self.alt || function_key)
    }

    /// The chord in two bytes, its key code and its modifiers, for a [`ShortcutOverrides`]
    /// slot. It is never [`DEFAULT_SLOT`] or [`UNBOUND_SLOT`]: every key code is at least `!`.
    fn code(self) -> u16 {
        let modifiers =
            u8::from(self.control) | u8::from(self.alt) << 1 | u8::from(self.shift) << 2;
        u16::from_le_bytes([self.key.0, modifiers])
    }

    const fn from_code(code: u16) -> Self {
        let [key, modifiers] = code.to_le_bytes();
        Self {
            control: modifiers & 1 != 0,
            alt: modifiers & 2 != 0,
            shift: modifiers & 4 != 0,
            key: ShortcutKey(key),
        }
    }
}

fn key_label(key: &str) -> String {
    match key {
        "pageup" => "Page Up".to_string(),
        "pagedown" => "Page Down".to_string(),
        key => {
            let mut characters = key.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        }
    }
}

/// What a shortcut runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutOwner {
    Command(CommandId),
    Workspace(WorkspaceShortcut),
}

impl ShortcutOwner {
    /// The name the shortcuts list shows for it.
    #[must_use]
    pub fn title(self) -> Text {
        match self {
            Self::Command(id) => command(id).short_title,
            Self::Workspace(shortcut) => shortcut.title(),
        }
    }
}

/// Why a key chord cannot run a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutRefusal {
    /// Another shortcut already uses the chord.
    Taken(ShortcutOwner),
    /// The operating system owns the chord.
    System,
    /// The chart consumes the chord.
    ChartKey,
    /// The chord has neither Ctrl nor Alt and is not a function key, so it would take a key
    /// away from typing.
    NeedsModifier,
    /// A modifier alone, or a key no shortcut can store.
    Unsupported,
    /// The command runs only from the command palette.
    NotBindable,
}

/// The first chord of a shortcut set that cannot be bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortcutConflict {
    pub owner: ShortcutOwner,
    pub chord: ShortcutChord,
    pub refusal: ShortcutRefusal,
}

impl fmt::Display for ShortcutConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chord = self.chord.label();
        let owner = self.owner.title();
        match self.refusal {
            ShortcutRefusal::Taken(other) => write!(
                formatter,
                "{chord} is bound to both {} and {owner}",
                other.title()
            ),
            ShortcutRefusal::System => write!(formatter, "{chord} ({owner}) is a system shortcut"),
            ShortcutRefusal::ChartKey => {
                write!(formatter, "{chord} ({owner}) shadows a chart key")
            }
            ShortcutRefusal::NeedsModifier => {
                write!(formatter, "{chord} ({owner}) needs Ctrl or Alt")
            }
            ShortcutRefusal::Unsupported => {
                write!(
                    formatter,
                    "{chord} ({owner}) is not a key a shortcut can use"
                )
            }
            ShortcutRefusal::NotBindable => {
                write!(
                    formatter,
                    "{owner} has the shortcut {chord} but no key action"
                )
            }
        }
    }
}

impl std::error::Error for ShortcutConflict {}

/// How the user changed one command's shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutOverride {
    /// The command has no shortcut.
    Unbound,
    /// This chord replaces the command's default keys.
    Chord(ShortcutChord),
}

/// The user's changes to the default command shortcuts: one optional slot per command, and a
/// command without one keeps its registry keys. Every edit is checked against the other
/// shortcuts, the system chords and the chart keys before it is returned, and restored
/// overrides drop what no longer fits, so a value here always binds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortcutOverrides {
    /// [`DEFAULT_SLOT`], [`UNBOUND_SLOT`] or a chord's code, two bytes per command so the
    /// chrome preferences holding them stay small enough to copy.
    slots: [u16; CommandId::COUNT],
}

/// The slot of a command that keeps its default keys.
const DEFAULT_SLOT: u16 = 0;
/// The slot of a command the user left without a shortcut.
const UNBOUND_SLOT: u16 = 1;

impl Default for ShortcutOverrides {
    fn default() -> Self {
        Self {
            slots: [DEFAULT_SLOT; CommandId::COUNT],
        }
    }
}

impl ShortcutOverrides {
    #[must_use]
    pub const fn get(&self, id: CommandId) -> Option<ShortcutOverride> {
        match self.slots[id.index()] {
            DEFAULT_SLOT => None,
            UNBOUND_SLOT => Some(ShortcutOverride::Unbound),
            code => Some(ShortcutOverride::Chord(ShortcutChord::from_code(code))),
        }
    }

    /// Whether the user changed `id` from its default keys.
    #[must_use]
    pub const fn is_customized(&self, id: CommandId) -> bool {
        self.slots[id.index()] != DEFAULT_SLOT
    }

    #[must_use]
    pub fn any_customized(&self) -> bool {
        self.slots.iter().any(|slot| *slot != DEFAULT_SLOT)
    }

    /// The chords that run `id`, the displayed one first.
    #[must_use]
    pub fn chords(&self, id: CommandId) -> Vec<ShortcutChord> {
        self.chords_of(command(id))
    }

    fn chords_of(&self, spec: &CommandSpec) -> Vec<ShortcutChord> {
        match self.get(spec.id) {
            None => spec.chords.to_vec(),
            Some(ShortcutOverride::Unbound) => Vec::new(),
            Some(ShortcutOverride::Chord(chord)) => vec![chord],
        }
    }

    /// Makes `chord` the one shortcut of `id`, in place of its default keys.
    ///
    /// # Errors
    ///
    /// Why `chord` cannot run `id`: another shortcut has it, the system or the chart owns it,
    /// it would take a key away from typing, or `id` runs only from the command palette.
    pub fn bind(
        &self,
        id: CommandId,
        chord: ShortcutChord,
        chart_keys: &[Keystroke],
    ) -> Result<Self, ShortcutRefusal> {
        if let Some(refusal) = chord_refusal(ShortcutOwner::Command(id), chord, chart_keys) {
            return Err(refusal);
        }
        if let Some(holder) = self.holder(chord, id) {
            return Err(ShortcutRefusal::Taken(holder));
        }
        Ok(self.with(id, ShortcutOverride::Chord(chord)))
    }

    /// Leaves `id` without a shortcut.
    #[must_use]
    pub fn unbind(&self, id: CommandId) -> Self {
        self.with(id, ShortcutOverride::Unbound)
    }

    /// Gives `id` its default keys back.
    ///
    /// # Errors
    ///
    /// [`ShortcutRefusal::Taken`] when another shortcut now uses one of them.
    pub fn reset(&self, id: CommandId) -> Result<Self, ShortcutRefusal> {
        if let Some(holder) = command(id)
            .chords
            .iter()
            .find_map(|chord| self.holder(*chord, id))
        {
            return Err(ShortcutRefusal::Taken(holder));
        }
        let mut next = *self;
        next.slots[id.index()] = DEFAULT_SLOT;
        Ok(next)
    }

    /// Stores `change` for `id`, or clears the slot when the change leaves the default keys.
    fn with(&self, id: CommandId, change: ShortcutOverride) -> Self {
        let defaults = command(id).chords;
        let default = match change {
            ShortcutOverride::Unbound => defaults.is_empty(),
            ShortcutOverride::Chord(chord) => defaults == [chord],
        };
        let mut next = *self;
        next.slots[id.index()] = match change {
            _ if default => DEFAULT_SLOT,
            ShortcutOverride::Unbound => UNBOUND_SLOT,
            ShortcutOverride::Chord(chord) => chord.code(),
        };
        next
    }

    /// The shortcut other than `except` that runs `chord`.
    fn holder(&self, chord: ShortcutChord, except: CommandId) -> Option<ShortcutOwner> {
        bound_chords(COMMANDS, self, WORKSPACE_SHORTCUTS)
            .into_iter()
            .find(|&(owner, bound)| bound == chord && owner != ShortcutOwner::Command(except))
            .map(|(owner, _)| owner)
    }

    /// Checks every desktop shortcut with these overrides applied; see [`validate_chords`].
    ///
    /// # Errors
    ///
    /// Returns the first chord that cannot be bound.
    pub fn validate(&self, chart_keys: &[Keystroke]) -> Result<(), ShortcutConflict> {
        validate_chords(COMMANDS, self, WORKSPACE_SHORTCUTS, chart_keys)
    }

    /// The saved form: one entry per changed command, in registry order.
    #[must_use]
    pub fn to_workspace(&self) -> Vec<WorkspaceShortcutOverrideState> {
        CommandId::ALL
            .into_iter()
            .filter_map(|id| {
                self.get(id).map(|change| WorkspaceShortcutOverrideState {
                    command: id.persisted_id().to_string(),
                    chord: match change {
                        ShortcutOverride::Unbound => String::new(),
                        ShortcutOverride::Chord(chord) => chord.source(),
                    },
                })
            })
            .collect()
    }

    /// Restores saved overrides. Unknown commands, commands no shortcut can run, chords that
    /// do not parse and repeated commands are skipped. An override that no longer fits beside
    /// the other shortcuts, the system chords and `chart_keys` is dropped, so its command keeps
    /// its default keys.
    #[must_use]
    pub fn from_workspace(
        saved: &[WorkspaceShortcutOverrideState],
        chart_keys: &[Keystroke],
    ) -> Self {
        let mut overrides = Self::default();
        for entry in saved {
            let Some(id) = CommandId::from_persisted_id(&entry.command) else {
                continue;
            };
            if !id.accepts_shortcut() || overrides.is_customized(id) {
                continue;
            }
            let change = if entry.chord.is_empty() {
                ShortcutOverride::Unbound
            } else if let Some(chord) = ShortcutChord::parse(&entry.chord) {
                ShortcutOverride::Chord(chord)
            } else {
                continue;
            };
            overrides = overrides.with(id, change);
        }
        overrides.drop_conflicts(chart_keys);
        overrides
    }

    /// Drops overrides until the whole set binds. Each round drops one, so the rounds end
    /// within one per command.
    fn drop_conflicts(&mut self, chart_keys: &[Keystroke]) {
        for _ in 0..CommandId::COUNT {
            let Err(conflict) = self.validate(chart_keys) else {
                return;
            };
            let other = match conflict.refusal {
                ShortcutRefusal::Taken(other) => Some(other),
                _ => None,
            };
            let culprit = [Some(conflict.owner), other]
                .into_iter()
                .flatten()
                .find_map(|owner| match owner {
                    ShortcutOwner::Command(id) if self.is_customized(id) => Some(id),
                    _ => None,
                });
            // Two defaults in conflict are a registry defect that the startup check reports.
            let Some(id) = culprit else {
                return;
            };
            self.slots[id.index()] = DEFAULT_SLOT;
        }
    }
}

const fn spec(
    id: CommandId,
    title: Text,
    keywords: Text,
    chords: &'static [ShortcutChord],
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
    chords: &'static [ShortcutChord],
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
        &[ShortcutChord::named("k").ctrl()],
    ),
    spec(
        CommandId::ToggleFullscreen,
        shell::CMD_TOGGLE_FULLSCREEN,
        shell::CMD_TOGGLE_FULLSCREEN_KEYWORDS,
        &[
            ShortcutChord::named("f11"),
            ShortcutChord::named("enter").alt(),
        ],
    ),
    spec(
        CommandId::MinimizeWindow,
        shell::CMD_MINIMIZE_WINDOW,
        shell::CMD_MINIMIZE_WINDOW_KEYWORDS,
        &[ShortcutChord::named("f9").alt()],
    ),
    spec(
        CommandId::ZoomWindow,
        shell::CMD_ZOOM_WINDOW,
        shell::CMD_ZOOM_WINDOW_KEYWORDS,
        &[ShortcutChord::named("f10").alt()],
    ),
    spec(
        CommandId::CloseWindow,
        shell::CMD_CLOSE_WINDOW,
        shell::CMD_CLOSE_WINDOW_KEYWORDS,
        &[ShortcutChord::named("f4").alt()],
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
        &[ShortcutChord::named("t").ctrl()],
    ),
    spec(
        CommandId::SelectNextWorkspace,
        shell::CMD_NEXT_WORKSPACE,
        shell::CMD_NEXT_WORKSPACE_KEYWORDS,
        &[ShortcutChord::named("tab").ctrl()],
    ),
    spec(
        CommandId::SelectPreviousWorkspace,
        shell::CMD_PREVIOUS_WORKSPACE,
        shell::CMD_PREVIOUS_WORKSPACE_KEYWORDS,
        &[ShortcutChord::named("tab").ctrl().shift()],
    ),
    spec(
        CommandId::MoveWorkspaceLeft,
        shell::CMD_MOVE_WORKSPACE_LEFT,
        shell::CMD_MOVE_WORKSPACE_LEFT_KEYWORDS,
        &[ShortcutChord::named("pageup").ctrl().shift()],
    ),
    spec(
        CommandId::MoveWorkspaceRight,
        shell::CMD_MOVE_WORKSPACE_RIGHT,
        shell::CMD_MOVE_WORKSPACE_RIGHT_KEYWORDS,
        &[ShortcutChord::named("pagedown").ctrl().shift()],
    ),
    spec(
        CommandId::CloseWorkspace,
        shell::CMD_CLOSE_WORKSPACE,
        shell::CMD_CLOSE_WORKSPACE_KEYWORDS,
        &[ShortcutChord::named("w").ctrl()],
    ),
    spec(
        CommandId::SaveLayout,
        shell::CMD_SAVE_LAYOUT,
        shell::CMD_SAVE_LAYOUT_KEYWORDS,
        &[ShortcutChord::named("s").ctrl()],
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
        &[ShortcutChord::named("h").ctrl().alt()],
    ),
    spec(
        CommandId::SplitVertical,
        shell::CMD_SPLIT_VERTICAL,
        shell::CMD_SPLIT_VERTICAL_KEYWORDS,
        &[ShortcutChord::named("v").ctrl().alt()],
    ),
    spec(
        CommandId::ClosePane,
        shell::CMD_CLOSE_PANE,
        shell::CMD_CLOSE_PANE_KEYWORDS,
        &[ShortcutChord::named("w").ctrl().shift()],
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
        &[ShortcutChord::named("b").ctrl()],
    ),
    prefixed_spec(
        CommandId::SellMarket,
        shell::CMD_SELL_MARKET,
        shell::CMD_SELL_MARKET_SHORT,
        shell::CMD_SELL_MARKET_KEYWORDS,
        &[ShortcutChord::named("s").ctrl().shift()],
    ),
    prefixed_spec(
        CommandId::CancelAll,
        shell::CMD_CANCEL_ALL,
        shell::CMD_CANCEL_ALL_SHORT,
        shell::CMD_CANCEL_ALL_KEYWORDS,
        &[ShortcutChord::named("x").ctrl().shift()],
    ),
    prefixed_spec(
        CommandId::FlattenAccount,
        shell::CMD_FLATTEN_ACCOUNT,
        shell::CMD_FLATTEN_ACCOUNT_SHORT,
        shell::CMD_FLATTEN_ACCOUNT_KEYWORDS,
        &[ShortcutChord::named("f").ctrl().shift()],
    ),
    prefixed_spec(
        CommandId::KillSwitch,
        shell::CMD_KILL_SWITCH,
        shell::CMD_KILL_SWITCH_SHORT,
        shell::CMD_KILL_SWITCH_KEYWORDS,
        &[ShortcutChord::named("k").ctrl().shift()],
    ),
];

/// Chords Windows owns. An application binding would never fire or would break a system habit.
/// No chord holds the Windows key, so a press with it is refused before it becomes one.
const SYSTEM_CHORDS: &[ShortcutChord] = &[
    ShortcutChord::named("tab").alt(),
    ShortcutChord::named("tab").alt().shift(),
    ShortcutChord::named("escape").alt(),
    ShortcutChord::named("space").alt(),
    ShortcutChord::named("escape").ctrl(),
    ShortcutChord::named("escape").ctrl().shift(),
    ShortcutChord::named("delete").ctrl().alt(),
];

/// Why `owner` cannot use `chord` whatever the other shortcuts are.
fn chord_refusal(
    owner: ShortcutOwner,
    chord: ShortcutChord,
    chart_keys: &[Keystroke],
) -> Option<ShortcutRefusal> {
    if SYSTEM_CHORDS.contains(&chord) {
        return Some(ShortcutRefusal::System);
    }
    if chart_keys.iter().any(|key| chord.matches(key)) {
        return Some(ShortcutRefusal::ChartKey);
    }
    match owner {
        ShortcutOwner::Command(id) if !id.accepts_shortcut() => Some(ShortcutRefusal::NotBindable),
        // Workspace shortcuts are matched by the workspace, which gives way to text fields, so
        // only the keymap's command chords need Ctrl or Alt.
        ShortcutOwner::Command(_) if chord.types_text() => Some(ShortcutRefusal::NeedsModifier),
        ShortcutOwner::Command(_) | ShortcutOwner::Workspace(_) => None,
    }
}

/// Every chord in use, in registry order: the commands with `overrides` applied, then the
/// workspace keys.
fn bound_chords(
    commands: &[CommandSpec],
    overrides: &ShortcutOverrides,
    workspace: &[WorkspaceShortcutSpec],
) -> Vec<(ShortcutOwner, ShortcutChord)> {
    let commands = commands.iter().flat_map(|spec| {
        overrides
            .chords_of(spec)
            .into_iter()
            .map(|chord| (ShortcutOwner::Command(spec.id), chord))
    });
    let workspace = workspace.iter().filter_map(|spec| match spec.trigger {
        ShortcutTrigger::Key(chord) => Some((ShortcutOwner::Workspace(spec.shortcut), chord)),
        ShortcutTrigger::AltClick => None,
    });
    commands.chain(workspace).collect()
}

/// Checks every key chord of `commands`, with `overrides` applied, and of `workspace`: no two
/// shortcuts share it, it is not a system chord, it does not shadow a key the chart consumes
/// (`chart_keys`, the key contract Aeris Charts publishes), only a command with a key action
/// has one, and a command chord holds Ctrl or Alt unless it is a function key.
///
/// # Errors
///
/// Returns the first chord that fails, naming the conflict.
pub fn validate_chords(
    commands: &[CommandSpec],
    overrides: &ShortcutOverrides,
    workspace: &[WorkspaceShortcutSpec],
    chart_keys: &[Keystroke],
) -> Result<(), ShortcutConflict> {
    let bound = bound_chords(commands, overrides, workspace);
    for (index, &(owner, chord)) in bound.iter().enumerate() {
        let refusal = chord_refusal(owner, chord, chart_keys).or_else(|| {
            bound[..index]
                .iter()
                .find(|(_, other)| *other == chord)
                .map(|&(other, _)| ShortcutRefusal::Taken(other))
        });
        if let Some(refusal) = refusal {
            return Err(ShortcutConflict {
                owner,
                chord,
                refusal,
            });
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

    const fn ctrl(key: &'static str) -> ShortcutChord {
        ShortcutChord::named(key).ctrl()
    }

    fn saved(command: &str, chord: &str) -> WorkspaceShortcutOverrideState {
        WorkspaceShortcutOverrideState {
            command: command.to_string(),
            chord: chord.to_string(),
        }
    }

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
        let mut persisted = std::collections::BTreeSet::new();
        for (index, id) in CommandId::ALL.into_iter().enumerate() {
            assert_eq!(
                id.index(),
                index,
                "ALL lists the commands in declaration order"
            );
            assert!(persisted.insert(id.persisted_id()), "{id:?}");
            assert_eq!(CommandId::from_persisted_id(id.persisted_id()), Some(id));
            assert!(
                id.accepts_shortcut() || command(id).chords.is_empty(),
                "{id:?} runs only from the palette, so it has no default shortcut"
            );
        }
        assert_eq!(CommandId::from_persisted_id("retired_command"), None);
    }

    #[test]
    fn chord_validation_rejects_duplicates_system_keys_and_chart_keys() {
        const CTRL_B: &[ShortcutChord] = &[ctrl("b")];
        const ALT_TAB: &[ShortcutChord] = &[ShortcutChord::named("tab").alt()];
        const BARE_B: &[ShortcutChord] = &[ShortcutChord::named("b")];
        const CTRL_M: &[ShortcutChord] = &[ctrl("m")];
        let none = ShortcutOverrides::default();
        assert_eq!(
            validate_chords(COMMANDS, &none, WORKSPACE_SHORTCUTS, &[]),
            Ok(())
        );
        let buy = command(CommandId::BuyMarket);
        let sell = CommandSpec {
            chords: CTRL_B,
            ..*command(CommandId::SellMarket)
        };
        let refusal = |commands: &[CommandSpec],
                       workspace: &[WorkspaceShortcutSpec],
                       chart_keys: &[Keystroke]| {
            validate_chords(commands, &none, workspace, chart_keys)
                .expect_err("conflict")
                .refusal
        };
        let conflict = validate_chords(&[*buy, sell], &none, &[], &[]).expect_err("duplicate");
        assert_eq!(
            conflict.refusal,
            ShortcutRefusal::Taken(ShortcutOwner::Command(CommandId::BuyMarket))
        );
        assert!(
            conflict.to_string().contains("Ctrl+B is bound to both"),
            "{conflict}"
        );
        let alt_tab = CommandSpec {
            chords: ALT_TAB,
            ..*buy
        };
        assert_eq!(refusal(&[alt_tab], &[], &[]), ShortcutRefusal::System);
        let bare = CommandSpec {
            chords: BARE_B,
            ..*buy
        };
        assert_eq!(refusal(&[bare], &[], &[]), ShortcutRefusal::NeedsModifier);
        let palette_only = CommandSpec {
            chords: CTRL_M,
            ..*command(CommandId::ToggleContext)
        };
        assert_eq!(
            refusal(&[palette_only], &[], &[]),
            ShortcutRefusal::NotBindable
        );
        let chart_key = Keystroke::parse("ctrl-b").expect("chart key");
        assert_eq!(
            refusal(&[*buy], &[], &[chart_key]),
            ShortcutRefusal::ChartKey
        );

        let fullscreen = WorkspaceShortcutSpec {
            trigger: ShortcutTrigger::Key(ctrl("b")),
            ..WORKSPACE_SHORTCUTS[1]
        };
        assert_eq!(
            refusal(&[*buy], &[fullscreen], &[]),
            ShortcutRefusal::Taken(ShortcutOwner::Command(CommandId::BuyMarket))
        );
        let space = Keystroke::parse("space").expect("chart key");
        let conflict =
            validate_chords(&[], &none, WORKSPACE_SHORTCUTS, &[space]).expect_err("shadowed");
        assert_eq!(conflict.refusal, ShortcutRefusal::ChartKey);
        assert!(
            conflict.to_string().contains("Next watchlist symbol"),
            "{conflict}"
        );
    }

    #[test]
    fn chords_read_back_what_they_write_and_refuse_what_no_shortcut_can_hold() {
        for source in [
            "ctrl-shift-b",
            "f11",
            "alt-enter",
            "ctrl--",
            "ctrl-alt-shift-pagedown",
        ] {
            let chord = ShortcutChord::parse(source).expect(source);
            assert_eq!(chord.source(), source);
            let typed =
                Keystroke::parse_with_os(source, gpui::OperatingSystem::Windows).expect(source);
            assert!(chord.matches(&typed), "{source}");
        }
        assert_eq!(
            ShortcutChord::parse("shift-ctrl-b"),
            Some(ctrl("b").shift()),
            "modifiers read in any order"
        );
        for invalid in [
            "",
            "ctrl-",
            "ctrl-ctrl-b",
            "cmd-b",
            "meta-b",
            "win-b",
            "B",
            "ctrl-shift",
            "shift",
            "ctrl-page-up",
            "ctrl-verylongkeynamepast16",
            "ctrl-f25",
            "ctrl-é",
        ] {
            assert_eq!(ShortcutChord::parse(invalid), None, "{invalid}");
        }
        let pressed = |source: &'static str| {
            ShortcutChord::from_keystroke(&Keystroke::parse(source).expect(source))
        };
        assert_eq!(pressed("ctrl-shift-b"), Ok(ctrl("b").shift()));
        assert_eq!(pressed("cmd-b"), Err(ShortcutRefusal::System));
        assert_eq!(pressed("ctrl"), Err(ShortcutRefusal::Unsupported));
    }

    #[test]
    fn every_storable_key_keeps_its_name_and_its_slot_code() {
        let names = super::PRINTABLE_KEYS
            .split("")
            .filter(|name| !name.is_empty() && !name.chars().any(char::is_uppercase))
            .chain(super::NAMED_KEYS.iter().copied());
        let mut count = 0;
        for name in names {
            for chord in [
                ShortcutChord::parse(name).expect(name),
                ShortcutChord::parse(name).expect(name).ctrl().alt().shift(),
            ] {
                assert_eq!(chord.key.name(), name);
                assert_eq!(ShortcutChord::from_code(chord.code()), chord, "{name}");
                assert!(chord.code() > super::UNBOUND_SLOT, "{name}");
            }
            count += 1;
        }
        assert_eq!(count, 68 + super::NAMED_KEYS.len());
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
        let none = ShortcutOverrides::default();
        assert_eq!(
            none.chords(CommandId::KillSwitch)
                .first()
                .map(ShortcutChord::label)
                .as_deref(),
            Some("Ctrl+Shift+K")
        );
        assert_eq!(
            ShortcutChord::named("pageup").ctrl().shift().label(),
            "Ctrl+Shift+Page Up"
        );
        assert_eq!(ShortcutChord::named("enter").alt().keys(), ["Alt", "Enter"]);
        assert_eq!(ShortcutChord::named("f11").label(), "F11");
        assert_eq!(
            none.chords(CommandId::ToggleContext),
            Vec::<ShortcutChord>::new()
        );
    }

    #[test]
    fn shortcut_sections_list_every_shortcut_once_trading_first() {
        let sections = shortcut_sections(&ShortcutOverrides::default());
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
                ("Toggle one-click trading", String::new()),
                ("Buy market", "Ctrl+B".to_string()),
                ("Sell market", "Ctrl+Shift+S".to_string()),
                ("Cancel all", "Ctrl+Shift+X".to_string()),
                ("Flatten account", "Ctrl+Shift+F".to_string()),
                ("Kill switch", "Ctrl+Shift+K".to_string()),
            ],
            "a command a shortcut can run is listed even without one, so it can get one"
        );
        assert!(
            trading
                .iter()
                .all(|row| row.command.is_some() && !row.customized)
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
        let bindable = CommandId::ALL
            .into_iter()
            .filter(|id| id.accepts_shortcut())
            .count();
        assert_eq!(listed, bindable + WORKSPACE_SHORTCUTS.len());
        assert_eq!(
            tr!(command(CommandId::NewWorkspace).short_title),
            "New workspace"
        );
        assert_eq!(
            tr!(command(CommandId::BuyMarket).short_title),
            "Buy market",
            "the short title is its own text, not a slice of the translated title"
        );

        let rebound = ShortcutOverrides::default()
            .bind(CommandId::BuyMarket, ctrl("b").shift(), &[])
            .expect("free chord");
        let trading = &shortcut_sections(&rebound)[0].1;
        let buy = trading
            .iter()
            .find(|row| row.command == Some(CommandId::BuyMarket))
            .expect("buy row");
        assert!(buy.customized);
        assert_eq!(buy.triggers, [ShortcutTrigger::Key(ctrl("b").shift())]);
    }

    #[test]
    fn overrides_bind_unbind_and_reset_only_what_fits_beside_the_other_shortcuts() {
        let chart_keys = [Keystroke::parse("ctrl-z").expect("chart key")];
        let none = ShortcutOverrides::default();
        let bind =
            |overrides: &ShortcutOverrides, id, chord| overrides.bind(id, chord, &chart_keys);
        assert_eq!(
            bind(&none, CommandId::BuyMarket, ctrl("s")),
            Err(ShortcutRefusal::Taken(ShortcutOwner::Command(
                CommandId::SaveLayout
            )))
        );
        assert_eq!(
            bind(
                &none,
                CommandId::BuyMarket,
                ShortcutChord::named("tab").alt()
            ),
            Err(ShortcutRefusal::System)
        );
        assert_eq!(
            bind(&none, CommandId::BuyMarket, ctrl("z")),
            Err(ShortcutRefusal::ChartKey)
        );
        assert_eq!(
            bind(
                &none,
                CommandId::BuyMarket,
                ShortcutChord::named("f").shift()
            ),
            Err(ShortcutRefusal::NeedsModifier),
            "Shift alone types a capital letter"
        );
        assert_eq!(
            bind(&none, CommandId::ToggleContext, ctrl("m")),
            Err(ShortcutRefusal::NotBindable)
        );
        let function_key = bind(
            &none,
            CommandId::ToggleOneClickTrading,
            ShortcutChord::named("f8"),
        )
        .expect("function keys need no modifier");
        assert_eq!(
            function_key.chords(CommandId::ToggleOneClickTrading),
            [ShortcutChord::named("f8")]
        );

        let moved = bind(&none, CommandId::BuyMarket, ctrl("b").shift()).expect("free chord");
        assert_eq!(moved.chords(CommandId::BuyMarket), [ctrl("b").shift()]);
        let swapped = bind(&moved, CommandId::SellMarket, ctrl("b")).expect("Ctrl+B is free now");
        assert_eq!(swapped.chords(CommandId::SellMarket), [ctrl("b")]);
        assert_eq!(
            swapped.reset(CommandId::BuyMarket),
            Err(ShortcutRefusal::Taken(ShortcutOwner::Command(
                CommandId::SellMarket
            ))),
            "a reset never takes a chord another command now holds"
        );
        let reset = swapped
            .reset(CommandId::SellMarket)
            .and_then(|overrides| overrides.reset(CommandId::BuyMarket))
            .expect("defaults fit again");
        assert_eq!(reset, none);
        assert_eq!(
            bind(&moved, CommandId::BuyMarket, ctrl("b")),
            Ok(none),
            "binding the default chord back is no change"
        );

        let unbound = none.unbind(CommandId::ToggleFullscreen);
        assert!(unbound.is_customized(CommandId::ToggleFullscreen));
        assert_eq!(
            unbound.chords(CommandId::ToggleFullscreen),
            Vec::<ShortcutChord>::new()
        );
        assert_eq!(
            none.unbind(CommandId::ToggleOneClickTrading),
            none,
            "a command without default keys is already unbound"
        );
        assert!(!none.any_customized() && unbound.any_customized());
        assert_eq!(swapped.validate(&chart_keys), Ok(()));
    }

    #[test]
    fn overrides_round_trip_their_saved_form_and_skip_what_they_cannot_use() {
        let chart_keys = [Keystroke::parse("ctrl-z").expect("chart key")];
        let overrides = ShortcutOverrides::default()
            .bind(CommandId::BuyMarket, ctrl("b").shift(), &chart_keys)
            .expect("free chord")
            .unbind(CommandId::KillSwitch);
        let saved_form = overrides.to_workspace();
        assert_eq!(
            saved_form,
            [
                saved("buy_market", "ctrl-shift-b"),
                saved("kill_switch", "")
            ],
            "an empty chord is an unbound command, an absent entry keeps the default"
        );
        let restored = ShortcutOverrides::from_workspace(&saved_form, &chart_keys);
        assert_eq!(restored, overrides);
        assert_eq!(
            restored.chords(CommandId::KillSwitch),
            Vec::<ShortcutChord>::new()
        );
        assert_eq!(
            restored.chords(CommandId::SellMarket),
            [ctrl("s").shift()],
            "a command without an entry keeps its default keys"
        );

        let restored = ShortcutOverrides::from_workspace(
            &[
                saved("retired_command", "ctrl-shift-b"),
                saved("toggle_context", "ctrl-m"),
                saved("buy_market", "meta-b"),
                saved("sell_market", "ctrl-banana-q"),
                saved("cancel_all", "ctrl-alt-c"),
                saved("cancel_all", "ctrl-alt-d"),
                saved("flatten_account", "ctrl-z"),
                saved("close_pane", "ctrl-s"),
                saved("kill_switch", "f"),
            ],
            &chart_keys,
        );
        assert_eq!(
            restored.to_workspace(),
            [saved("cancel_all", "ctrl-alt-c")],
            "unknown and palette-only commands, unreadable chords, repeats and chords that \
             clash with the chart, another command or typing are skipped"
        );
        assert_eq!(restored.chords(CommandId::BuyMarket), [ctrl("b")]);

        let swapped = ShortcutOverrides::from_workspace(
            &[
                saved("buy_market", "ctrl-shift-s"),
                saved("sell_market", "ctrl-b"),
            ],
            &chart_keys,
        );
        assert_eq!(
            swapped.chords(CommandId::BuyMarket),
            [ctrl("s").shift()],
            "two commands that swapped keys restore together whatever their order"
        );
        assert_eq!(swapped.chords(CommandId::SellMarket), [ctrl("b")]);
        assert_eq!(swapped.validate(&chart_keys), Ok(()));
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
