use super::*;
use ChartDrawingKind as Kind;
use assets::DrawingIcon as Glyph;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct DrawingToolbarState {
    pub(super) availability: DrawingToolbarAvailability,
    /// The armed Aeris Charts tool; `None` is the cursor.
    pub(super) active_tool: Option<ChartDrawingKind>,
    pub(super) drawing_count: usize,
    pub(super) selection: DrawingToolbarSelection,
    pub(super) selected_locked: bool,
    pub(super) time_axis_height: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DrawingToolbarAvailability {
    #[default]
    Unavailable,
    Available,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DrawingToolbarSelection {
    #[default]
    None,
    Drawing,
    Series,
}

impl DrawingToolbarState {
    pub(super) fn from_chart(chart: &AerisChartView) -> Self {
        let selection = if chart.selected_drawing_id().is_some() {
            DrawingToolbarSelection::Drawing
        } else if chart.has_deletable_selection() {
            DrawingToolbarSelection::Series
        } else {
            DrawingToolbarSelection::None
        };
        Self {
            availability: DrawingToolbarAvailability::Available,
            active_tool: chart.drawing_tool(),
            drawing_count: chart.drawing_count(),
            selection,
            selected_locked: chart.selected_drawing_locked(),
            time_axis_height: chart.time_axis_height(),
        }
    }
}

/// One toolbar command: the cursor, an Aeris Charts drawing tool, or the icon-stamp tool armed
/// with one built-in stamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DrawingToolChoice {
    Cursor,
    Kind(ChartDrawingKind),
    Stamp(ChartDrawingStamp),
}

#[derive(Clone, Copy)]
enum DrawingToolGlyph {
    Asset(assets::DrawingIcon),
    Stamp(ChartDrawingStamp),
}

#[derive(Clone, Copy)]
struct DrawingToolEntry {
    choice: DrawingToolChoice,
    label: &'static str,
    glyph: DrawingToolGlyph,
}

impl DrawingToolEntry {
    const fn tool(kind: ChartDrawingKind, label: &'static str, icon: assets::DrawingIcon) -> Self {
        Self {
            choice: DrawingToolChoice::Kind(kind),
            label,
            glyph: DrawingToolGlyph::Asset(icon),
        }
    }

    const fn stamp(stamp: ChartDrawingStamp) -> Self {
        Self {
            choice: DrawingToolChoice::Stamp(stamp),
            label: stamp.label(),
            glyph: DrawingToolGlyph::Stamp(stamp),
        }
    }

    /// Stamps carry their chart color; tool glyphs follow the control's text color.
    fn icon(self, theme: &AerisTheme) -> Icon {
        match self.glyph {
            DrawingToolGlyph::Asset(icon) => Icon::new(icon.path()),
            DrawingToolGlyph::Stamp(stamp) => {
                Icon::new(assets::stamp_icon_path(stamp)).color(gpui_color(stamp.color(theme)))
            }
        }
    }

    /// Toolbar glyph size: the cursor, brush, and text glyphs and the solid stamps read heavier
    /// than the thin line art, so they render smaller.
    const fn toolbar_icon_size(self) -> f32 {
        match self.choice {
            DrawingToolChoice::Cursor
            | DrawingToolChoice::Kind(ChartDrawingKind::Brush | ChartDrawingKind::Text) => 24.0,
            DrawingToolChoice::Stamp(_) => 22.0,
            DrawingToolChoice::Kind(_) => 28.0,
        }
    }
}

struct DrawingToolSection {
    title: &'static str,
    tools: &'static [DrawingToolEntry],
}

struct DrawingToolGroup {
    id: &'static str,
    menu_id: &'static str,
    label: &'static str,
    sections: &'static [DrawingToolSection],
}

impl DrawingToolGroup {
    fn entries(&self) -> impl Iterator<Item = &'static DrawingToolEntry> + use<> {
        let sections: &'static [DrawingToolSection] = self.sections;
        sections.iter().flat_map(|section| section.tools.iter())
    }

    fn first(&self) -> DrawingToolEntry {
        self.sections[0].tools[0]
    }

    fn has_menu(&self) -> bool {
        self.entries().nth(1).is_some()
    }

    fn entry(&self, choice: DrawingToolChoice) -> Option<DrawingToolEntry> {
        self.entries().find(|entry| entry.choice == choice).copied()
    }
}

const DRAWING_TOOL_GROUP_COUNT: usize = 8;

/// The sidebar groups, top to bottom. Every Aeris Charts drawing kind appears exactly once; the
/// icon-stamp kind appears once per built-in stamp.
static DRAWING_TOOL_GROUPS: [DrawingToolGroup; DRAWING_TOOL_GROUP_COUNT] = [
    DrawingToolGroup {
        id: "drawing_group_cursor",
        menu_id: "drawing_group_cursor_menu",
        label: "Cursor",
        sections: &[DrawingToolSection {
            title: "Cursor",
            tools: &[DrawingToolEntry {
                choice: DrawingToolChoice::Cursor,
                label: "Cursor",
                glyph: DrawingToolGlyph::Asset(Glyph::Cursor),
            }],
        }],
    },
    DrawingToolGroup {
        id: "drawing_group_lines",
        menu_id: "drawing_group_lines_menu",
        label: "Lines, channels, and pitchforks",
        sections: &[
            DrawingToolSection {
                title: "Lines",
                tools: &[
                    DrawingToolEntry::tool(Kind::TrendLine, "Trend line", Glyph::TrendLine),
                    DrawingToolEntry::tool(Kind::Ray, "Ray", Glyph::Ray),
                    DrawingToolEntry::tool(Kind::InfoLine, "Info line", Glyph::InfoLine),
                    DrawingToolEntry::tool(
                        Kind::ExtendedLine,
                        "Extended line",
                        Glyph::ExtendedLine,
                    ),
                    DrawingToolEntry::tool(Kind::TrendAngle, "Trend angle", Glyph::TrendAngle),
                    DrawingToolEntry::tool(
                        Kind::HorizontalLine,
                        "Horizontal line",
                        Glyph::HorizontalLine,
                    ),
                    DrawingToolEntry::tool(
                        Kind::HorizontalRay,
                        "Horizontal ray",
                        Glyph::HorizontalRay,
                    ),
                    DrawingToolEntry::tool(
                        Kind::VerticalLine,
                        "Vertical line",
                        Glyph::VerticalLine,
                    ),
                    DrawingToolEntry::tool(Kind::CrossLine, "Cross line", Glyph::CrossLine),
                    DrawingToolEntry::tool(Kind::ArrowLine, "Arrow", Glyph::ArrowLine),
                ],
            },
            DrawingToolSection {
                title: "Channels",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::ParallelChannel,
                        "Parallel channel",
                        Glyph::ParallelChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::RegressionTrend,
                        "Regression trend",
                        Glyph::RegressionTrend,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FlatTopChannel,
                        "Flat top channel",
                        Glyph::FlatTopChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FlatBottomChannel,
                        "Flat bottom channel",
                        Glyph::FlatBottomChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::DisjointChannel,
                        "Disjoint channel",
                        Glyph::DisjointChannel,
                    ),
                ],
            },
            DrawingToolSection {
                title: "Pitchforks",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::AndrewsPitchfork,
                        "Pitchfork",
                        Glyph::AndrewsPitchfork,
                    ),
                    DrawingToolEntry::tool(
                        Kind::SchiffPitchfork,
                        "Schiff pitchfork",
                        Glyph::SchiffPitchfork,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ModifiedSchiffPitchfork,
                        "Modified Schiff pitchfork",
                        Glyph::ModifiedSchiffPitchfork,
                    ),
                    DrawingToolEntry::tool(
                        Kind::InsidePitchfork,
                        "Inside pitchfork",
                        Glyph::InsidePitchfork,
                    ),
                    DrawingToolEntry::tool(Kind::Pitchfan, "Pitchfan", Glyph::Pitchfan),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_fibonacci",
        menu_id: "drawing_group_fibonacci_menu",
        label: "Fibonacci and Gann",
        sections: &[
            DrawingToolSection {
                title: "Fibonacci",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::FibonacciRetracement,
                        "Fib retracement",
                        Glyph::FibonacciRetracement,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciExtension,
                        "Trend-based fib extension",
                        Glyph::FibonacciExtension,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciChannel,
                        "Fib channel",
                        Glyph::FibonacciChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciTimeZones,
                        "Fib time zone",
                        Glyph::FibonacciTimeZones,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciTrendTime,
                        "Trend-based fib time",
                        Glyph::FibonacciTrendTime,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciSpeedFan,
                        "Fib speed resistance fan",
                        Glyph::FibonacciSpeedFan,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciSpeedArcs,
                        "Fib speed resistance arcs",
                        Glyph::FibonacciSpeedArcs,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciCircles,
                        "Fib circles",
                        Glyph::FibonacciCircles,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciSpiral,
                        "Fib spiral",
                        Glyph::FibonacciSpiral,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciWedge,
                        "Fib wedge",
                        Glyph::FibonacciWedge,
                    ),
                ],
            },
            DrawingToolSection {
                title: "Gann",
                tools: &[
                    DrawingToolEntry::tool(Kind::GannBox, "Gann box", Glyph::GannBox),
                    DrawingToolEntry::tool(
                        Kind::GannSquareFixed,
                        "Gann square fixed",
                        Glyph::GannSquareFixed,
                    ),
                    DrawingToolEntry::tool(Kind::GannSquare, "Gann square", Glyph::GannSquare),
                    DrawingToolEntry::tool(Kind::GannFan, "Gann fan", Glyph::GannFan),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_patterns",
        menu_id: "drawing_group_patterns_menu",
        label: "Patterns",
        sections: &[
            DrawingToolSection {
                title: "Chart patterns",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::PatternXabcd,
                        "XABCD pattern",
                        Glyph::PatternXabcd,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternCypher,
                        "Cypher pattern",
                        Glyph::PatternCypher,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternHeadShoulders,
                        "Head and shoulders",
                        Glyph::PatternHeadShoulders,
                    ),
                    DrawingToolEntry::tool(Kind::PatternAbcd, "ABCD pattern", Glyph::PatternAbcd),
                    DrawingToolEntry::tool(
                        Kind::PatternTriangle,
                        "Triangle pattern",
                        Glyph::PatternTriangle,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternThreeDrives,
                        "Three drives pattern",
                        Glyph::PatternThreeDrives,
                    ),
                ],
            },
            DrawingToolSection {
                title: "Elliott waves",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::ElliottImpulse,
                        "Elliott impulse wave (12345)",
                        Glyph::ElliottImpulse,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottCorrection,
                        "Elliott correction wave (ABC)",
                        Glyph::ElliottCorrection,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottTriangle,
                        "Elliott triangle wave (ABCDE)",
                        Glyph::ElliottTriangle,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottDoubleCombination,
                        "Elliott double combo wave (WXY)",
                        Glyph::ElliottDoubleCombination,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottTripleCombination,
                        "Elliott triple combo wave (WXYXZ)",
                        Glyph::ElliottTripleCombination,
                    ),
                ],
            },
            DrawingToolSection {
                title: "Cycles",
                tools: &[
                    DrawingToolEntry::tool(Kind::CyclicLines, "Cyclic lines", Glyph::CyclicLines),
                    DrawingToolEntry::tool(Kind::TimeCycles, "Time cycles", Glyph::TimeCycles),
                    DrawingToolEntry::tool(Kind::SineLine, "Sine line", Glyph::SineLine),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_forecasting",
        menu_id: "drawing_group_forecasting_menu",
        label: "Forecasting and measurement",
        sections: &[
            DrawingToolSection {
                title: "Forecasting",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::LongPosition,
                        "Long position",
                        Glyph::LongPosition,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ShortPosition,
                        "Short position",
                        Glyph::ShortPosition,
                    ),
                    DrawingToolEntry::tool(Kind::Forecast, "Forecast", Glyph::Forecast),
                    DrawingToolEntry::tool(Kind::BarsPattern, "Bars pattern", Glyph::BarsPattern),
                    DrawingToolEntry::tool(Kind::Projection, "Projection", Glyph::Projection),
                ],
            },
            DrawingToolSection {
                title: "Volume-based",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::AnchoredVwap,
                        "Anchored VWAP",
                        Glyph::AnchoredVwap,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FixedRangeVolumeProfile,
                        "Fixed range volume profile",
                        Glyph::FixedRangeVolumeProfile,
                    ),
                    DrawingToolEntry::tool(
                        Kind::AnchoredVolumeProfile,
                        "Anchored volume profile",
                        Glyph::AnchoredVolumeProfile,
                    ),
                ],
            },
            DrawingToolSection {
                title: "Measurers",
                tools: &[
                    DrawingToolEntry::tool(Kind::PriceRange, "Price range", Glyph::PriceRange),
                    DrawingToolEntry::tool(Kind::DateRange, "Date range", Glyph::DateRange),
                    DrawingToolEntry::tool(
                        Kind::DatePriceRange,
                        "Date and price range",
                        Glyph::DatePriceRange,
                    ),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_shapes",
        menu_id: "drawing_group_shapes_menu",
        label: "Brushes and shapes",
        sections: &[
            DrawingToolSection {
                title: "Brushes",
                tools: &[
                    DrawingToolEntry::tool(Kind::Brush, "Brush", Glyph::Brush),
                    DrawingToolEntry::tool(Kind::Highlighter, "Highlighter", Glyph::Highlighter),
                ],
            },
            DrawingToolSection {
                title: "Shapes",
                tools: &[
                    DrawingToolEntry::tool(Kind::Rectangle, "Rectangle", Glyph::Rectangle),
                    DrawingToolEntry::tool(
                        Kind::RotatedRectangle,
                        "Rotated rectangle",
                        Glyph::RotatedRectangle,
                    ),
                    DrawingToolEntry::tool(Kind::Path, "Path", Glyph::Path),
                    DrawingToolEntry::tool(Kind::Circle, "Circle", Glyph::Circle),
                    DrawingToolEntry::tool(Kind::Ellipse, "Ellipse", Glyph::Ellipse),
                    DrawingToolEntry::tool(Kind::Polyline, "Polyline", Glyph::Polyline),
                    DrawingToolEntry::tool(Kind::Triangle, "Triangle", Glyph::Triangle),
                    DrawingToolEntry::tool(Kind::Arc, "Arc", Glyph::Arc),
                    DrawingToolEntry::tool(Kind::Curve, "Curve", Glyph::Curve),
                    DrawingToolEntry::tool(Kind::DoubleCurve, "Double curve", Glyph::DoubleCurve),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_text",
        menu_id: "drawing_group_text_menu",
        label: "Text and notes",
        sections: &[DrawingToolSection {
            title: "Text and notes",
            tools: &[
                DrawingToolEntry::tool(Kind::Text, "Text", Glyph::Text),
                DrawingToolEntry::tool(Kind::AnchoredText, "Anchored text", Glyph::AnchoredText),
                DrawingToolEntry::tool(Kind::Note, "Note", Glyph::Note),
                DrawingToolEntry::tool(Kind::PriceNote, "Price note", Glyph::PriceNote),
                DrawingToolEntry::tool(Kind::Callout, "Callout", Glyph::Callout),
                DrawingToolEntry::tool(Kind::Comment, "Comment", Glyph::Comment),
                DrawingToolEntry::tool(Kind::PriceLabel, "Price label", Glyph::PriceLabel),
                DrawingToolEntry::tool(Kind::Signpost, "Signpost", Glyph::Signpost),
                DrawingToolEntry::tool(Kind::FlagMark, "Flag mark", Glyph::FlagMark),
            ],
        }],
    },
    DrawingToolGroup {
        id: "drawing_group_markers",
        menu_id: "drawing_group_markers_menu",
        label: "Arrows and stamps",
        sections: &[
            DrawingToolSection {
                title: "Stamps",
                tools: &[
                    DrawingToolEntry::stamp(ChartDrawingStamp::Check),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Cross),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Star),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Alert),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Info),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Question),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Bolt),
                    DrawingToolEntry::stamp(ChartDrawingStamp::Target),
                ],
            },
            DrawingToolSection {
                title: "Arrows",
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerUp,
                        "Arrow mark up",
                        Glyph::ArrowMarkerUp,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerDown,
                        "Arrow mark down",
                        Glyph::ArrowMarkerDown,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerLeft,
                        "Arrow mark left",
                        Glyph::ArrowMarkerLeft,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerRight,
                        "Arrow mark right",
                        Glyph::ArrowMarkerRight,
                    ),
                ],
            },
        ],
    },
];

fn drawing_tool_group_of(choice: DrawingToolChoice) -> Option<usize> {
    DRAWING_TOOL_GROUPS
        .iter()
        .position(|group| group.entry(choice).is_some())
}

/// Sidebar presentation memory: the tool each group slot shows (its most recently chosen one)
/// and the open group flyout. The armed tool itself is always read back from Aeris Charts.
pub(super) struct DrawingToolMenu {
    recent: [DrawingToolChoice; DRAWING_TOOL_GROUP_COUNT],
    open: Option<usize>,
    slot_bounds: [Option<Bounds<Pixels>>; DRAWING_TOOL_GROUP_COUNT],
}

impl Default for DrawingToolMenu {
    fn default() -> Self {
        Self {
            recent: std::array::from_fn(|group| DRAWING_TOOL_GROUPS[group].first().choice),
            open: None,
            slot_bounds: [None; DRAWING_TOOL_GROUP_COUNT],
        }
    }
}

impl DrawingToolMenu {
    /// Makes `choice` its group's slot tool and closes the flyout.
    pub(super) fn record(&mut self, choice: DrawingToolChoice) {
        if let Some(group) = drawing_tool_group_of(choice) {
            self.recent[group] = choice;
        }
        self.open = None;
    }

    pub(super) fn toggle(&mut self, group: usize) {
        self.open = (self.open != Some(group)).then_some(group);
    }

    /// Closes the flyout, returning whether one was open.
    pub(super) fn close(&mut self) -> bool {
        self.open.take().is_some()
    }

    pub(super) const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Records where a slot was laid out, returning whether its open flyout must follow it.
    fn track_slot(&mut self, group: usize, bounds: Bounds<Pixels>) -> bool {
        let moved = self.slot_bounds[group] != Some(bounds);
        self.slot_bounds[group] = Some(bounds);
        moved && self.open == Some(group)
    }

    /// The toolbar command matching the tool Aeris Charts has armed. The engine reports the
    /// icon-stamp kind without its stamp, which the toolbar chose most recently.
    pub(super) fn armed_choice(&self, active: Option<ChartDrawingKind>) -> DrawingToolChoice {
        match active {
            None => DrawingToolChoice::Cursor,
            Some(ChartDrawingKind::IconStamp) => self
                .recent
                .iter()
                .copied()
                .find(|choice| matches!(choice, DrawingToolChoice::Stamp(_)))
                .unwrap_or(DrawingToolChoice::Stamp(ChartDrawingStamp::ALL[0])),
            Some(kind) => DrawingToolChoice::Kind(kind),
        }
    }

    /// The tool a slot shows: the armed tool when it belongs to the group, otherwise the group's
    /// most recent choice.
    fn shown(&self, group: usize, armed: DrawingToolChoice) -> DrawingToolEntry {
        let tools = &DRAWING_TOOL_GROUPS[group];
        tools
            .entry(armed)
            .or_else(|| tools.entry(self.recent[group]))
            .unwrap_or_else(|| tools.first())
    }
}

impl DrawingToolChoice {
    /// The cursor is always on the sidebar, so only drawing tools and stamps can be starred.
    fn is_favorite(self, favorites: &chart_chrome::DrawingFavorites) -> bool {
        match self {
            Self::Cursor => false,
            Self::Kind(kind) => favorites.contains_kind(kind),
            Self::Stamp(stamp) => favorites.contains_stamp(stamp),
        }
    }
}

/// Starred tools in sidebar order, so the floating toolbar reads like the groups it came from.
fn favorite_entries(
    favorites: &chart_chrome::DrawingFavorites,
) -> impl Iterator<Item = DrawingToolEntry> + '_ {
    DRAWING_TOOL_GROUPS
        .iter()
        .flat_map(DrawingToolGroup::entries)
        .copied()
        .filter(|entry| entry.choice.is_favorite(favorites))
}

/// Drag payload for moving the favorites toolbar by its grip.
#[derive(Clone)]
struct DrawingFavoritesMoveDrag;

impl Render for DrawingFavoritesMoveDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

impl TerminalApp {
    pub(super) fn toggle_drawing_favorite(
        &mut self,
        choice: DrawingToolChoice,
        cx: &mut Context<Self>,
    ) {
        let favorites = &mut self.chart_chrome.drawing_favorites;
        let first = favorites.is_empty();
        match choice {
            DrawingToolChoice::Cursor => return,
            DrawingToolChoice::Kind(kind) => favorites.toggle_kind(kind),
            DrawingToolChoice::Stamp(stamp) => favorites.toggle_stamp(stamp),
        }
        if first {
            favorites.toolbar_visible = true;
        }
        self.commit_drawing_favorites(cx);
    }

    pub(super) fn toggle_drawing_favorites_toolbar(&mut self, cx: &mut Context<Self>) {
        let favorites = &mut self.chart_chrome.drawing_favorites;
        favorites.toolbar_visible = !favorites.toolbar_visible;
        self.commit_drawing_favorites(cx);
    }

    fn begin_drawing_favorites_move(
        &mut self,
        pointer: gpui::Point<Pixels>,
        origin: gpui::Point<Pixels>,
    ) {
        self.drawing_favorites_grab = Some(pointer - origin);
    }

    /// Follows the pointer in memory only; the position is saved once when the drag ends.
    fn move_drawing_favorites(
        &mut self,
        pointer: gpui::Point<Pixels>,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(grab) = self.drawing_favorites_grab else {
            return;
        };
        let favorites = &mut self.chart_chrome.drawing_favorites;
        let count = favorite_entries(favorites).count();
        let toolbar = drawing_favorites_toolbar_size(count, self.theme.dimensions.border_width);
        let origin = clamp_floating_panel_origin(pointer - grab, viewport, toolbar);
        let origin = Some((f32::from(origin.x), f32::from(origin.y)));
        if favorites.toolbar_origin != origin {
            favorites.toolbar_origin = origin;
            cx.notify();
        }
    }

    pub(super) fn end_drawing_favorites_move(&mut self, cx: &mut Context<Self>) {
        if self.drawing_favorites_grab.take().is_some() {
            self.commit_drawing_favorites(cx);
        }
    }

    /// Every surface saves its own copy of the chrome preferences, so each one must hold the
    /// current favorites before any of them writes the shared file.
    fn commit_drawing_favorites(&mut self, cx: &mut Context<Self>) {
        let favorites = self.chart_chrome.drawing_favorites;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.drawing_favorites = favorites;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }
}

#[derive(Clone, Copy)]
struct DrawingToolSlot {
    group: usize,
    entry: DrawingToolEntry,
    selected: bool,
    menu_open: bool,
    enabled: bool,
}

/// What the expanded sidebar shows besides the active chart's drawing state.
#[derive(Clone, Copy)]
pub(super) struct DrawingSidebar<'a> {
    pub(super) menu: &'a DrawingToolMenu,
    pub(super) favorites: chart_chrome::DrawingFavorites,
}

pub(super) fn drawing_toolbar(
    terminal: Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: DrawingToolbarState,
    sidebar: DrawingSidebar<'_>,
    scroll: &ScrollHandle,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let menu = sidebar.menu;
    let colors = theme.colors;
    let armed = menu.armed_choice(state.active_tool);
    let enabled = state.availability == DrawingToolbarAvailability::Available;
    let tools = (0..DRAWING_TOOL_GROUP_COUNT)
        .map(|group| DrawingToolSlot {
            group,
            entry: menu.shown(group, armed),
            selected: DRAWING_TOOL_GROUPS[group].entry(armed).is_some(),
            menu_open: menu.open == Some(group),
            enabled,
        })
        .map(|slot| drawing_tool_slot(&terminal, slot, theme));
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(px(chart_chrome::CHART_CHROME_HEIGHT))
        .flex()
        .flex_col()
        .items_center()
        .overflow_hidden()
        .border_r_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .relative()
                .flex_1()
                .w_full()
                .min_h(px(0.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .py_2()
                        .size_full()
                        .min_h(px(0.0))
                        .map(|body| tracked_overflow_y_scrollbar(body, scroll))
                        .children(tools)
                        .child(drawing_favorites_toggle(
                            &terminal,
                            sidebar.favorites,
                            theme,
                        ))
                        .child(drawing_toolbar_actions(app, state, theme)),
                )
                .child(ThinScrollbar::new(
                    scroll,
                    gpui_color(colors.text_secondary),
                )),
        )
        .child(drawing_toolbar_collapse(
            terminal,
            state.time_axis_height,
            theme,
        ))
}

const DRAWING_TOOL_MENU_WIDTH: f32 = 264.0;
const DRAWING_TOOL_MENU_GAP: f32 = 4.0;
const DRAWING_TOOL_MENU_ICON: f32 = 24.0;
const DRAWING_TOOL_MENU_ROW_REMS: f32 = 2.0;
const DRAWING_TOOL_MENU_TITLE_REMS: f32 = 1.75;
const DRAWING_TOOL_BUTTON_SIZE: f32 = 32.0;
const DRAWING_TOOL_GROUP_ARROW_HEIGHT: f32 = 12.0;
const DRAWING_TOOL_GROUP_ARROW_ICON: f32 = 10.0;

/// One sidebar slot: the group's shown tool arms on click, and groups with more than one tool
/// open their flyout from a separate arrow strip under the button.
fn drawing_tool_slot(
    terminal: &Entity<TerminalApp>,
    slot: DrawingToolSlot,
    theme: &AerisTheme,
) -> AnyElement {
    let group = &DRAWING_TOOL_GROUPS[slot.group];
    let entry = slot.entry;
    let arm = terminal.clone();
    let button = drawing_toolbar_action(
        drawing_toolbar_button(
            group.id,
            entry.icon(theme),
            entry.label,
            entry.toolbar_icon_size(),
            theme,
            slot.selected,
        ),
        slot.enabled,
    );
    let button = chrome_tooltip(
        group.id,
        entry.label,
        button_activation(button, slot.enabled, move |_, cx| {
            arm.update(cx, |terminal, terminal_cx| {
                terminal.select_drawing_tool_on_active_workspace(entry.choice, terminal_cx);
            });
        }),
        theme,
    );
    let bounds_terminal = terminal.clone();
    let index = slot.group;
    div()
        .relative()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .child(button)
        .when(group.has_menu(), |container| {
            container.child(drawing_tool_group_arrow(terminal, slot, theme))
        })
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_terminal.update(cx, |terminal, terminal_cx| {
                        if terminal.drawing_tool_menu.track_slot(index, bounds) {
                            terminal_cx.notify();
                        }
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}

fn drawing_tool_group_arrow(
    terminal: &Entity<TerminalApp>,
    slot: DrawingToolSlot,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let group = &DRAWING_TOOL_GROUPS[slot.group];
    let spec = TooltipSpec::new(group.label, theme).show_delay(TOOLTIP_OPEN_DELAY);
    let toggle = terminal.clone();
    let index = slot.group;
    div()
        .id(group.menu_id)
        .flex_none()
        .w(px(DRAWING_TOOL_BUTTON_SIZE))
        .h(px(DRAWING_TOOL_GROUP_ARROW_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )))
        .role(Role::Button)
        .aria_label(group.label)
        .text_color(gpui_color(if slot.menu_open {
            colors.icon_active
        } else {
            colors.text_muted
        }))
        .when(slot.menu_open, |arrow| {
            arrow.bg(gpui_color(colors.active_bg.over(colors.surface)))
        })
        .when(slot.enabled, |arrow| {
            arrow
                .cursor_pointer()
                .hover(move |arrow| {
                    arrow
                        .bg(gpui_color(colors.hover_bg.over(colors.surface)))
                        .text_color(gpui_color(colors.text_primary))
                })
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    toggle.update(cx, |terminal, terminal_cx| {
                        terminal.toggle_drawing_tool_menu(index, terminal_cx);
                    });
                    cx.stop_propagation();
                })
        })
        .when(!slot.enabled, Styled::cursor_not_allowed)
        .tooltip(spec.builder())
        .tooltip_show_delay(spec.delay())
        .child(Icon::new(Glyph::GroupArrow.path()).with_size(px(DRAWING_TOOL_GROUP_ARROW_ICON)))
}

/// The open group's flyout, anchored beside its sidebar slot. Rows and titles have fixed rem
/// heights, so the panel clamps into the window without measuring a frame first.
pub(super) fn drawing_tool_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &DrawingToolMenu,
    favorites: &chart_chrome::DrawingFavorites,
    active_tool: Option<ChartDrawingKind>,
    viewport: gpui::Size<Pixels>,
    rem_size: Pixels,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let index = menu.open?;
    let trigger = menu.slot_bounds[index]?;
    let group = &DRAWING_TOOL_GROUPS[index];
    let armed = menu.armed_choice(active_tool);
    let panel_size = size(
        px(DRAWING_TOOL_MENU_WIDTH),
        drawing_tool_menu_height(group, rem_size, theme.dimensions.border_width),
    );
    let origin = drawing_tool_menu_origin(trigger, panel_size, viewport);
    let mut panel = flat_compact_menu_panel(
        ("drawing_tool_menu", index),
        origin,
        panel_size.width,
        theme,
    )
    .max_h((viewport.height - px(2.0 * OVERLAY_EDGE_MARGIN)).max(px(0.0)))
    .overflow_y_scroll();
    let mut row = 0_usize;
    for (section_index, section) in group.sections.iter().enumerate() {
        if section_index > 0 {
            panel = panel.child(menu_separator(theme));
        }
        panel = panel.child(drawing_tool_menu_title(section.title, theme));
        let last_section = section_index + 1 == group.sections.len();
        for (tool_index, entry) in section.tools.iter().copied().enumerate() {
            let select = terminal.clone();
            let last = last_section && tool_index + 1 == section.tools.len();
            panel = panel.child(
                MenuRow::compact(("drawing_tool_menu_row", row), entry.label, theme)
                    .leading(entry.icon(theme).with_size(px(DRAWING_TOOL_MENU_ICON)))
                    .trailing(drawing_favorite_star(
                        terminal,
                        row,
                        entry.choice,
                        entry.choice.is_favorite(favorites),
                        theme,
                    ))
                    .highlighted(entry.choice == armed)
                    .flush_in_panel(false, last)
                    .on_click(move |_, _, cx| {
                        select.update(cx, |terminal, terminal_cx| {
                            terminal
                                .select_drawing_tool_on_active_workspace(entry.choice, terminal_cx);
                        });
                    }),
            );
            row += 1;
        }
    }
    let dismiss = terminal.clone();
    Some(
        div()
            .id("drawing_tool_menu_scrim")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .on_any_mouse_down(move |_, _, cx| {
                dismiss.update(cx, |terminal, terminal_cx| {
                    terminal.close_drawing_tool_menu(terminal_cx);
                });
                cx.stop_propagation();
            })
            .child(animate_popup_from_origin(
                panel,
                ("drawing_tool_menu_enter", index),
                PopupAnimationOrigin::from_trigger(
                    trigger.center(),
                    Bounds::new(origin, panel_size),
                ),
            ))
            .into_any_element(),
    )
}

const DRAWING_FAVORITES_PADDING: f32 = 4.0;
const DRAWING_FAVORITES_GAP: f32 = 2.0;
const DRAWING_FAVORITES_GRIP_WIDTH: f32 = 14.0;
const DRAWING_FAVORITES_GRIP_DOT: f32 = 3.0;
/// Where the favorites toolbar opens until it is first dragged: just inside the chart's top-left.
const DRAWING_FAVORITES_DEFAULT_INSET: f32 = 12.0;

/// Fixed geometry, so moves clamp the toolbar into the window without measuring a frame.
fn drawing_favorites_toolbar_size(count: usize, border_width: f32) -> gpui::Size<Pixels> {
    let count = f32::from(u16::try_from(count).unwrap_or(u16::MAX));
    let edge = 2.0 * (DRAWING_FAVORITES_PADDING + border_width);
    size(
        px(edge
            + DRAWING_FAVORITES_GRIP_WIDTH
            + count * (DRAWING_FAVORITES_GAP + DRAWING_TOOL_BUTTON_SIZE)),
        px(edge + DRAWING_TOOL_BUTTON_SIZE),
    )
}

/// The default spot beside the drawing sidebar and below the chart header.
pub(super) fn drawing_favorites_default_origin(chart_top: f32) -> gpui::Point<Pixels> {
    point(
        px(chart_chrome::CHART_CHROME_HEIGHT + DRAWING_FAVORITES_DEFAULT_INSET),
        px(chart_top + DRAWING_FAVORITES_DEFAULT_INSET),
    )
}

/// Inputs for the floating favorites toolbar, read from the terminal before it renders.
pub(super) struct DrawingFavoritesToolbar<'a> {
    pub(super) favorites: chart_chrome::DrawingFavorites,
    pub(super) menu: &'a DrawingToolMenu,
    /// The active workspace's chart, whose armed tool the toolbar highlights.
    pub(super) state: DrawingToolbarState,
    pub(super) default_origin: gpui::Point<Pixels>,
    pub(super) viewport: gpui::Size<Pixels>,
}

/// The floating toolbar of starred tools. It sits above the workspace anywhere in the window,
/// moves by its grip, and arms a tool on the active workspace like the sidebar does.
pub(super) fn drawing_favorites_toolbar_layer(
    terminal: &Entity<TerminalApp>,
    toolbar: &DrawingFavoritesToolbar<'_>,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let favorites = &toolbar.favorites;
    if !favorites.toolbar_visible {
        return None;
    }
    let entries: Vec<_> = favorite_entries(favorites).collect();
    if entries.is_empty() {
        return None;
    }
    let armed = toolbar.menu.armed_choice(toolbar.state.active_tool);
    let enabled = toolbar.state.availability == DrawingToolbarAvailability::Available;
    let colors = theme.colors;
    let panel_size = drawing_favorites_toolbar_size(entries.len(), theme.dimensions.border_width);
    let stored = favorites
        .toolbar_origin
        .map_or(toolbar.default_origin, |(x, y)| point(px(x), px(y)));
    let origin = clamp_floating_panel_origin(stored, toolbar.viewport, panel_size);
    let buttons = entries.into_iter().enumerate().map(|(index, entry)| {
        let arm = terminal.clone();
        let button = drawing_toolbar_action(
            drawing_toolbar_button(
                ("drawing_favorite", index),
                entry.icon(theme),
                entry.label,
                entry.toolbar_icon_size(),
                theme,
                entry.choice == armed,
            ),
            enabled,
        );
        let button = button_activation(button, enabled, move |_, cx| {
            arm.update(cx, |terminal, terminal_cx| {
                terminal.select_drawing_tool_on_active_workspace(entry.choice, terminal_cx);
            });
        });
        let tooltip = TooltipSpec::new(entry.label, theme).show_delay(TOOLTIP_OPEN_DELAY);
        with_tooltip(("drawing_favorite_tooltip", index), button, &tooltip).into_any_element()
    });
    let move_terminal = terminal.clone();
    Some(
        div()
            .id("drawing_favorites_toolbar")
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(panel_size.width)
            .h(panel_size.height)
            .flex()
            .items_center()
            .gap(px(DRAWING_FAVORITES_GAP))
            .px(px(DRAWING_FAVORITES_PADDING))
            .rounded(px(f32::from(RadiusToken::Medium.logical_pixels())))
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_drag_move::<DrawingFavoritesMoveDrag>(move |event, window, cx| {
                let viewport = window.viewport_size();
                move_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.move_drawing_favorites(event.event.position, viewport, terminal_cx);
                });
            })
            .child(drawing_favorites_grip(terminal, origin, theme))
            .children(buttons)
            .into_any_element(),
    )
}

fn drawing_favorites_grip(
    terminal: &Entity<TerminalApp>,
    origin: gpui::Point<Pixels>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let dot = || {
        div()
            .size(px(DRAWING_FAVORITES_GRIP_DOT))
            .rounded_full()
            .bg(gpui_color(theme.colors.text_muted))
    };
    let column = || {
        div()
            .flex()
            .flex_col()
            .gap(px(DRAWING_FAVORITES_GRIP_DOT))
            .child(dot())
            .child(dot())
            .child(dot())
    };
    let spec = TooltipSpec::new("Move favorites toolbar", theme).show_delay(TOOLTIP_OPEN_DELAY);
    let grab = terminal.clone();
    div()
        .id("drawing_favorites_grip")
        .flex_none()
        .w(px(DRAWING_FAVORITES_GRIP_WIDTH))
        .h(px(DRAWING_TOOL_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(DRAWING_FAVORITES_GRIP_DOT))
        .cursor(FLOATING_PANEL_MOVE_CURSOR)
        .role(Role::Button)
        .aria_label("Move favorites toolbar")
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            grab.update(cx, |terminal, _| {
                terminal.begin_drawing_favorites_move(event.position, origin);
            });
        })
        .on_drag(DrawingFavoritesMoveDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
        .tooltip(spec.builder())
        .tooltip_show_delay(spec.delay())
        .child(column())
        .child(column())
}

const DRAWING_FAVORITE_STAR_SIZE: f32 = 24.0;
const DRAWING_FAVORITE_STAR_ICON: f32 = 16.0;

/// The row's own star target. It handles the press itself so starring a tool neither arms it
/// nor closes the flyout.
fn drawing_favorite_star(
    terminal: &Entity<TerminalApp>,
    row: usize,
    choice: DrawingToolChoice,
    favorite: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let label = if favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    let spec = TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY);
    let toggle = terminal.clone();
    div()
        .id(("drawing_favorite_star", row))
        .flex_none()
        .size(px(DRAWING_FAVORITE_STAR_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(label)
        .text_color(gpui_color(if favorite {
            colors.warning
        } else {
            colors.text_muted
        }))
        .hover(move |star| {
            star.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
                .text_color(gpui_color(if favorite {
                    colors.warning
                } else {
                    colors.text_primary
                }))
        })
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_drawing_favorite(choice, terminal_cx);
            });
            cx.stop_propagation();
        })
        .on_click(|_, _, cx| cx.stop_propagation())
        .tooltip(spec.builder())
        .tooltip_show_delay(spec.delay())
        .child(
            Icon::new(if favorite {
                HugeIcon::StarFilled.path()
            } else {
                HugeIcon::Star.path()
            })
            .with_size(px(DRAWING_FAVORITE_STAR_ICON)),
        )
}

fn drawing_tool_menu_title(title: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .h(gpui::rems(DRAWING_TOOL_MENU_TITLE_REMS))
        .flex()
        .items_center()
        .px(gpui::rems(0.75))
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(title)
}

fn drawing_tool_menu_height(
    group: &DrawingToolGroup,
    rem_size: Pixels,
    border_width: f32,
) -> Pixels {
    let rem = f32::from(rem_size);
    let sections = group.sections.len();
    let rows = group.entries().count();
    let count = |value: usize| f32::from(u16::try_from(value).unwrap_or(u16::MAX));
    px(count(rows) * DRAWING_TOOL_MENU_ROW_REMS * rem
        + count(sections) * DRAWING_TOOL_MENU_TITLE_REMS * rem
        + count(sections.saturating_sub(1))
        + 2.0 * border_width)
}

/// Opens to the right of the slot, top-aligned with it, sliding up when the window is too short.
fn drawing_tool_menu_origin(
    trigger: Bounds<Pixels>,
    panel: gpui::Size<Pixels>,
    viewport: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    let margin = px(OVERLAY_EDGE_MARGIN);
    let max_x = (viewport.width - panel.width - margin).max(margin);
    let max_y = (viewport.height - panel.height - margin).max(margin);
    point(
        (trigger.right() + px(DRAWING_TOOL_MENU_GAP)).min(max_x),
        trigger.top().min(max_y).max(margin),
    )
}

/// Shows or hides the floating favorites toolbar; it has nothing to show until a tool is starred.
fn drawing_favorites_toggle(
    terminal: &Entity<TerminalApp>,
    favorites: chart_chrome::DrawingFavorites,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let enabled = !favorites.is_empty();
    let shown = enabled && favorites.toolbar_visible;
    let label = if !enabled {
        "Star a drawing tool to add it to favorites"
    } else if shown {
        "Hide favorites toolbar"
    } else {
        "Show favorites toolbar"
    };
    let toggle = terminal.clone();
    let button = drawing_toolbar_action(
        drawing_toolbar_button(
            "drawing_favorites_toggle",
            header_icon(if shown {
                HugeIcon::StarFilled
            } else {
                HugeIcon::Star
            }),
            label,
            20.0,
            theme,
            shown,
        ),
        enabled,
    );
    chrome_tooltip(
        "drawing_favorites_toggle",
        label,
        button_activation(button, enabled, move |_, cx| {
            toggle.update(cx, TerminalApp::toggle_drawing_favorites_toolbar);
        }),
        theme,
    )
}

fn drawing_toolbar_actions(
    app: &Entity<WorkspaceSurface>,
    state: DrawingToolbarState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .py_2()
        .w_full()
        .border_t_1()
        .border_color(gpui_color(theme.colors.border))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_delete_selected",
                "Delete selected chart object",
                HugeIcon::Trash,
                24.0,
                false,
                state.selection != DrawingToolbarSelection::None,
                WorkspaceSurface::remove_selected_chart_object,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_lock_selected",
                "Lock or unlock selected drawing",
                HugeIcon::Lock,
                20.0,
                state.selected_locked,
                state.selection == DrawingToolbarSelection::Drawing,
                WorkspaceSurface::toggle_selected_drawing_lock,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_clear_all",
                "Clear all drawings",
                HugeIcon::EraserIcon,
                24.0,
                false,
                state.drawing_count > 0,
                WorkspaceSurface::clear_drawings,
            ),
            app.clone(),
            theme,
        ))
}

const DRAWING_TOOLBAR_TOGGLE_ICON: f32 = 14.0;

fn drawing_toolbar_toggle_height(time_axis_height: f32) -> f32 {
    // Aeris Charts reserves the complete time strip inside the chart. The desktop pane then
    // adds its bottom layout inset outside that canvas, so the adjacent control must span
    // both regions to match the visible X-axis row from top border to workspace edge.
    time_axis_height + WORKSPACE_PANE_BOTTOM_INSET
}

fn drawing_toolbar_collapse(
    terminal: Entity<TerminalApp>,
    time_axis_height: f32,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let height = drawing_toolbar_toggle_height(time_axis_height);
    drawing_toolbar_toggle_hit(
        "drawing_toolbar_collapse",
        HugeIcon::SidebarLeft,
        "Collapse drawing toolbar",
        theme,
        move |_, cx| terminal.update(cx, TerminalApp::toggle_drawing_toolbar),
    )
    .flex_none()
    .w_full()
    .h(px(height))
    .border_t_1()
    .border_color(gpui_color(colors.border))
}

#[derive(Clone, Copy)]
struct DrawingActionSpec {
    id: &'static str,
    tooltip: &'static str,
    icon: HugeIcon,
    icon_size: f32,
    selected: bool,
    enabled: bool,
    action: fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
}

impl DrawingActionSpec {
    const fn new(
        id: &'static str,
        tooltip: &'static str,
        icon: HugeIcon,
        icon_size: f32,
        selected: bool,
        enabled: bool,
        action: fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
    ) -> Self {
        Self {
            id,
            tooltip,
            icon,
            icon_size,
            selected,
            enabled,
            action,
        }
    }
}

fn drawing_action_control(
    spec: DrawingActionSpec,
    app: Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let button = drawing_toolbar_button(
        spec.id,
        header_icon(spec.icon),
        spec.tooltip,
        spec.icon_size,
        theme,
        spec.selected,
    );
    let button = drawing_toolbar_action(button, spec.enabled);
    let button = button_activation(button, spec.enabled, move |_, cx| {
        app.update(cx, spec.action);
    });
    chrome_tooltip(spec.id, spec.tooltip, button, theme)
}

pub(super) fn drawing_toolbar_expander(
    terminal: Entity<TerminalApp>,
    time_axis_height: f32,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let height = drawing_toolbar_toggle_height(time_axis_height);
    drawing_toolbar_toggle_hit(
        "drawing_toolbar_expand",
        HugeIcon::SidebarLeft,
        "Expand drawing toolbar",
        theme,
        move |_, cx| terminal.update(cx, TerminalApp::toggle_drawing_toolbar),
    )
    .absolute()
    .left_0()
    .bottom_0()
    .w(px(chart_chrome::CHART_CHROME_HEIGHT))
    .h(px(height))
    .border_t_1()
    .border_r_1()
    .border_color(gpui_color(colors.border))
    .bg(gpui_color(colors.surface))
}

fn drawing_toolbar_toggle_hit(
    id: &'static str,
    icon: HugeIcon,
    tooltip: &'static str,
    theme: &AerisTheme,
    on_activate: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let colors = theme.colors;
    let spec = TooltipSpec::new(tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY);
    div()
        .id(id)
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(0.0))
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(tooltip)
        .hover(move |hit| hit.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_activate(window, cx);
            cx.stop_propagation();
        })
        .tooltip(spec.builder())
        .tooltip_show_delay(spec.delay())
        .child(header_icon(icon).with_size(px(DRAWING_TOOLBAR_TOGGLE_ICON)))
}

fn drawing_toolbar_button(
    id: impl Into<gpui::ElementId>,
    icon: Icon,
    label: &'static str,
    icon_size: f32,
    theme: &AerisTheme,
    selected: bool,
) -> Button {
    let button = Button::new(id)
        .icon(icon)
        .aria_label(label)
        .compact()
        .with_size(px(icon_size / 0.75))
        .w(px(DRAWING_TOOL_BUTTON_SIZE))
        .h(px(DRAWING_TOOL_BUTTON_SIZE))
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )));
    chrome_button_style(button, theme, selected, true)
}

fn drawing_toolbar_action(button: Button, enabled: bool) -> Button {
    button
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_aeris_charts_drawing_tool_has_exactly_one_sidebar_entry() {
        let kinds: Vec<_> = (0..=u8::MAX)
            .filter_map(ChartDrawingKind::from_u8)
            .collect();
        assert!(
            kinds.len() >= 85,
            "Aeris Charts exposes {} tools",
            kinds.len()
        );
        let choices: Vec<_> = DRAWING_TOOL_GROUPS
            .iter()
            .flat_map(DrawingToolGroup::entries)
            .map(|entry| entry.choice)
            .collect();
        for kind in kinds {
            let expected = if kind == ChartDrawingKind::IconStamp {
                ChartDrawingStamp::ALL.len()
            } else {
                1
            };
            let listed = choices
                .iter()
                .filter(|choice| match choice {
                    DrawingToolChoice::Kind(listed) => *listed == kind,
                    DrawingToolChoice::Stamp(_) => kind == ChartDrawingKind::IconStamp,
                    DrawingToolChoice::Cursor => false,
                })
                .count();
            assert_eq!(listed, expected, "{kind:?} is listed {listed} times");
        }
        for stamp in ChartDrawingStamp::ALL {
            assert_eq!(
                choices
                    .iter()
                    .filter(|choice| **choice == DrawingToolChoice::Stamp(stamp))
                    .count(),
                1,
                "{stamp:?}"
            );
        }
        assert_eq!(
            choices
                .iter()
                .filter(|choice| **choice == DrawingToolChoice::Cursor)
                .count(),
            1
        );
        assert!(
            choices.iter().all(|choice| !matches!(
                choice,
                DrawingToolChoice::Kind(ChartDrawingKind::IconStamp)
            )),
            "the icon-stamp tool is listed by stamp"
        );
    }

    #[test]
    fn slots_show_the_armed_tool_and_otherwise_their_last_choice() {
        let mut menu = DrawingToolMenu::default();
        let lines = drawing_tool_group_of(DrawingToolChoice::Kind(ChartDrawingKind::TrendLine))
            .expect("lines group");
        let fibonacci = drawing_tool_group_of(DrawingToolChoice::Kind(
            ChartDrawingKind::FibonacciRetracement,
        ))
        .expect("fibonacci group");
        assert_eq!(
            menu.shown(lines, DrawingToolChoice::Cursor).choice,
            DrawingToolChoice::Kind(ChartDrawingKind::TrendLine)
        );

        menu.record(DrawingToolChoice::Kind(ChartDrawingKind::Pitchfan));
        let armed = menu.armed_choice(Some(ChartDrawingKind::FibonacciWedge));
        assert_eq!(
            menu.shown(lines, armed).choice,
            DrawingToolChoice::Kind(ChartDrawingKind::Pitchfan)
        );
        assert_eq!(
            menu.shown(fibonacci, armed).choice,
            DrawingToolChoice::Kind(ChartDrawingKind::FibonacciWedge)
        );
    }

    #[test]
    fn the_armed_icon_stamp_reads_back_as_the_last_chosen_stamp() {
        let mut menu = DrawingToolMenu::default();
        assert_eq!(
            menu.armed_choice(Some(ChartDrawingKind::IconStamp)),
            DrawingToolChoice::Stamp(ChartDrawingStamp::ALL[0])
        );
        menu.record(DrawingToolChoice::Stamp(ChartDrawingStamp::Bolt));
        assert_eq!(
            menu.armed_choice(Some(ChartDrawingKind::IconStamp)),
            DrawingToolChoice::Stamp(ChartDrawingStamp::Bolt)
        );
        assert_eq!(menu.armed_choice(None), DrawingToolChoice::Cursor);
    }

    #[test]
    fn favorites_toolbar_lists_starred_tools_in_sidebar_order() {
        let mut favorites = chart_chrome::DrawingFavorites::default();
        favorites.toggle_stamp(ChartDrawingStamp::Star);
        favorites.toggle_kind(ChartDrawingKind::FibonacciRetracement);
        favorites.toggle_kind(ChartDrawingKind::TrendLine);
        assert!(!DrawingToolChoice::Cursor.is_favorite(&favorites));
        assert_eq!(
            favorite_entries(&favorites)
                .map(|entry| entry.choice)
                .collect::<Vec<_>>(),
            [
                DrawingToolChoice::Kind(ChartDrawingKind::TrendLine),
                DrawingToolChoice::Kind(ChartDrawingKind::FibonacciRetracement),
                DrawingToolChoice::Stamp(ChartDrawingStamp::Star),
            ]
        );
    }

    #[test]
    fn favorites_toolbar_grows_one_button_per_starred_tool() {
        let one = drawing_favorites_toolbar_size(1, 1.0);
        let three = drawing_favorites_toolbar_size(3, 1.0);
        assert_eq!(
            one.height,
            px(2.0 * (DRAWING_FAVORITES_PADDING + 1.0) + 32.0)
        );
        assert_eq!(
            three.width - one.width,
            px(2.0 * (DRAWING_FAVORITES_GAP + DRAWING_TOOL_BUTTON_SIZE))
        );
        assert_eq!(three.height, one.height);
    }

    #[test]
    fn choosing_a_tool_closes_the_open_flyout() {
        let mut menu = DrawingToolMenu::default();
        menu.toggle(2);
        assert!(menu.is_open());
        menu.toggle(2);
        assert!(!menu.is_open());
        menu.toggle(3);
        menu.record(DrawingToolChoice::Kind(ChartDrawingKind::GannFan));
        assert!(!menu.is_open());
    }

    #[test]
    fn flyouts_open_beside_their_slot_and_stay_inside_the_window() {
        let viewport = size(px(1200.0), px(700.0));
        let slot = Bounds::new(point(px(0.0), px(120.0)), size(px(44.0), px(32.0)));
        let short = size(px(DRAWING_TOOL_MENU_WIDTH), px(200.0));
        assert_eq!(
            drawing_tool_menu_origin(slot, short, viewport),
            point(px(44.0 + DRAWING_TOOL_MENU_GAP), px(120.0))
        );
        let tall = size(px(DRAWING_TOOL_MENU_WIDTH), px(650.0));
        assert_eq!(
            drawing_tool_menu_origin(slot, tall, viewport).y,
            px(700.0 - 650.0 - OVERLAY_EDGE_MARGIN)
        );
        let lines = &DRAWING_TOOL_GROUPS[1];
        let height = drawing_tool_menu_height(lines, px(16.0), 1.0);
        let rows = 20.0 * 32.0;
        let titles = 3.0 * 28.0;
        assert_eq!(height, px(rows + titles + 2.0 + 2.0));
    }

    #[test]
    fn drawing_toggle_spans_the_engine_axis_and_host_bottom_inset() {
        let height = drawing_toolbar_toggle_height(22.0);
        assert!((height - 24.0).abs() < f32::EPSILON);
    }
}
