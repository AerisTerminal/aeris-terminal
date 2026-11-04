use super::*;
use crate::desktop::native_ui::tooltip::TooltipPlacement;
use ChartDrawingKind as Kind;
use aeris_localization::{Text, drawing, tr};
use assets::DrawingIcon as Glyph;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct DrawingToolbarState {
    pub(super) availability: DrawingToolbarAvailability,
    /// The armed Aeris Charts tool; `None` is the cursor.
    pub(super) active_tool: Option<ChartDrawingKind>,
    pub(super) drawing_count: usize,
    pub(super) selection: DrawingToolbarSelection,
    pub(super) selected_locked: bool,
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
    label: Text,
    glyph: DrawingToolGlyph,
}

impl DrawingToolEntry {
    const fn tool(kind: ChartDrawingKind, label: Text, icon: assets::DrawingIcon) -> Self {
        Self {
            choice: DrawingToolChoice::Kind(kind),
            label,
            glyph: DrawingToolGlyph::Asset(icon),
        }
    }

    const fn stamp(stamp: ChartDrawingStamp) -> Self {
        Self {
            choice: DrawingToolChoice::Stamp(stamp),
            label: stamp_text(stamp),
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
    title: Text,
    tools: &'static [DrawingToolEntry],
}

struct DrawingToolGroup {
    id: &'static str,
    menu_id: &'static str,
    label: Text,
    sections: &'static [DrawingToolSection],
}

/// The sidebar's name for a built-in stamp, translated like the tool labels.
const fn stamp_text(stamp: ChartDrawingStamp) -> Text {
    match stamp {
        ChartDrawingStamp::Check => drawing::STAMP_CHECK,
        ChartDrawingStamp::Cross => drawing::STAMP_CROSS,
        ChartDrawingStamp::Star => drawing::STAMP_STAR,
        ChartDrawingStamp::Alert => drawing::STAMP_ALERT,
        ChartDrawingStamp::Info => drawing::STAMP_INFO,
        ChartDrawingStamp::Question => drawing::STAMP_QUESTION,
        ChartDrawingStamp::Bolt => drawing::STAMP_BOLT,
        ChartDrawingStamp::Target => drawing::STAMP_TARGET,
    }
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
        label: drawing::CURSOR,
        sections: &[DrawingToolSection {
            title: drawing::CURSOR,
            tools: &[DrawingToolEntry {
                choice: DrawingToolChoice::Cursor,
                label: drawing::CURSOR,
                glyph: DrawingToolGlyph::Asset(Glyph::Cursor),
            }],
        }],
    },
    DrawingToolGroup {
        id: "drawing_group_lines",
        menu_id: "drawing_group_lines_menu",
        label: drawing::GROUP_LINES,
        sections: &[
            DrawingToolSection {
                title: drawing::LINES,
                tools: &[
                    DrawingToolEntry::tool(Kind::TrendLine, drawing::TREND_LINE, Glyph::TrendLine),
                    DrawingToolEntry::tool(Kind::Ray, drawing::RAY, Glyph::Ray),
                    DrawingToolEntry::tool(Kind::InfoLine, drawing::INFO_LINE, Glyph::InfoLine),
                    DrawingToolEntry::tool(
                        Kind::ExtendedLine,
                        drawing::EXTENDED_LINE,
                        Glyph::ExtendedLine,
                    ),
                    DrawingToolEntry::tool(
                        Kind::TrendAngle,
                        drawing::TREND_ANGLE,
                        Glyph::TrendAngle,
                    ),
                    DrawingToolEntry::tool(
                        Kind::HorizontalLine,
                        drawing::HORIZONTAL_LINE,
                        Glyph::HorizontalLine,
                    ),
                    DrawingToolEntry::tool(
                        Kind::HorizontalRay,
                        drawing::HORIZONTAL_RAY,
                        Glyph::HorizontalRay,
                    ),
                    DrawingToolEntry::tool(
                        Kind::VerticalLine,
                        drawing::VERTICAL_LINE,
                        Glyph::VerticalLine,
                    ),
                    DrawingToolEntry::tool(Kind::CrossLine, drawing::CROSS_LINE, Glyph::CrossLine),
                    DrawingToolEntry::tool(Kind::ArrowLine, drawing::ARROW, Glyph::ArrowLine),
                ],
            },
            DrawingToolSection {
                title: drawing::CHANNELS,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::ParallelChannel,
                        drawing::PARALLEL_CHANNEL,
                        Glyph::ParallelChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::RegressionTrend,
                        drawing::REGRESSION_TREND,
                        Glyph::RegressionTrend,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FlatTopChannel,
                        drawing::FLAT_TOP_CHANNEL,
                        Glyph::FlatTopChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FlatBottomChannel,
                        drawing::FLAT_BOTTOM_CHANNEL,
                        Glyph::FlatBottomChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::DisjointChannel,
                        drawing::DISJOINT_CHANNEL,
                        Glyph::DisjointChannel,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::PITCHFORKS,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::AndrewsPitchfork,
                        drawing::PITCHFORK,
                        Glyph::AndrewsPitchfork,
                    ),
                    DrawingToolEntry::tool(
                        Kind::SchiffPitchfork,
                        drawing::SCHIFF_PITCHFORK,
                        Glyph::SchiffPitchfork,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ModifiedSchiffPitchfork,
                        drawing::MODIFIED_SCHIFF_PITCHFORK,
                        Glyph::ModifiedSchiffPitchfork,
                    ),
                    DrawingToolEntry::tool(
                        Kind::InsidePitchfork,
                        drawing::INSIDE_PITCHFORK,
                        Glyph::InsidePitchfork,
                    ),
                    DrawingToolEntry::tool(Kind::Pitchfan, drawing::PITCHFAN, Glyph::Pitchfan),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_fibonacci",
        menu_id: "drawing_group_fibonacci_menu",
        label: drawing::GROUP_FIBONACCI,
        sections: &[
            DrawingToolSection {
                title: drawing::FIBONACCI,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::FibonacciRetracement,
                        drawing::FIB_RETRACEMENT,
                        Glyph::FibonacciRetracement,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciExtension,
                        drawing::TREND_BASED_FIB_EXTENSION,
                        Glyph::FibonacciExtension,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciChannel,
                        drawing::FIB_CHANNEL,
                        Glyph::FibonacciChannel,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciTimeZones,
                        drawing::FIB_TIME_ZONE,
                        Glyph::FibonacciTimeZones,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciTrendTime,
                        drawing::TREND_BASED_FIB_TIME,
                        Glyph::FibonacciTrendTime,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciSpeedFan,
                        drawing::FIB_SPEED_RESISTANCE_FAN,
                        Glyph::FibonacciSpeedFan,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciSpeedArcs,
                        drawing::FIB_SPEED_RESISTANCE_ARCS,
                        Glyph::FibonacciSpeedArcs,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciCircles,
                        drawing::FIB_CIRCLES,
                        Glyph::FibonacciCircles,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciSpiral,
                        drawing::FIB_SPIRAL,
                        Glyph::FibonacciSpiral,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FibonacciWedge,
                        drawing::FIB_WEDGE,
                        Glyph::FibonacciWedge,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::GANN,
                tools: &[
                    DrawingToolEntry::tool(Kind::GannBox, drawing::GANN_BOX, Glyph::GannBox),
                    DrawingToolEntry::tool(
                        Kind::GannSquareFixed,
                        drawing::GANN_SQUARE_FIXED,
                        Glyph::GannSquareFixed,
                    ),
                    DrawingToolEntry::tool(
                        Kind::GannSquare,
                        drawing::GANN_SQUARE,
                        Glyph::GannSquare,
                    ),
                    DrawingToolEntry::tool(Kind::GannFan, drawing::GANN_FAN, Glyph::GannFan),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_patterns",
        menu_id: "drawing_group_patterns_menu",
        label: drawing::GROUP_PATTERNS,
        sections: &[
            DrawingToolSection {
                title: drawing::CHART_PATTERNS,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::PatternXabcd,
                        drawing::XABCD_PATTERN,
                        Glyph::PatternXabcd,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternCypher,
                        drawing::CYPHER_PATTERN,
                        Glyph::PatternCypher,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternHeadShoulders,
                        drawing::HEAD_AND_SHOULDERS,
                        Glyph::PatternHeadShoulders,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternAbcd,
                        drawing::ABCD_PATTERN,
                        Glyph::PatternAbcd,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternTriangle,
                        drawing::TRIANGLE_PATTERN,
                        Glyph::PatternTriangle,
                    ),
                    DrawingToolEntry::tool(
                        Kind::PatternThreeDrives,
                        drawing::THREE_DRIVES_PATTERN,
                        Glyph::PatternThreeDrives,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::ELLIOTT_WAVES,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::ElliottImpulse,
                        drawing::ELLIOTT_IMPULSE_WAVE,
                        Glyph::ElliottImpulse,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottCorrection,
                        drawing::ELLIOTT_CORRECTION_WAVE,
                        Glyph::ElliottCorrection,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottTriangle,
                        drawing::ELLIOTT_TRIANGLE_WAVE,
                        Glyph::ElliottTriangle,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottDoubleCombination,
                        drawing::ELLIOTT_DOUBLE_COMBO_WAVE,
                        Glyph::ElliottDoubleCombination,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ElliottTripleCombination,
                        drawing::ELLIOTT_TRIPLE_COMBO_WAVE,
                        Glyph::ElliottTripleCombination,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::CYCLES,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::CyclicLines,
                        drawing::CYCLIC_LINES,
                        Glyph::CyclicLines,
                    ),
                    DrawingToolEntry::tool(
                        Kind::TimeCycles,
                        drawing::TIME_CYCLES,
                        Glyph::TimeCycles,
                    ),
                    DrawingToolEntry::tool(Kind::SineLine, drawing::SINE_LINE, Glyph::SineLine),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_forecasting",
        menu_id: "drawing_group_forecasting_menu",
        label: drawing::GROUP_FORECASTING,
        sections: &[
            DrawingToolSection {
                title: drawing::FORECASTING,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::LongPosition,
                        drawing::LONG_POSITION,
                        Glyph::LongPosition,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ShortPosition,
                        drawing::SHORT_POSITION,
                        Glyph::ShortPosition,
                    ),
                    DrawingToolEntry::tool(Kind::Forecast, drawing::FORECAST, Glyph::Forecast),
                    DrawingToolEntry::tool(
                        Kind::BarsPattern,
                        drawing::BARS_PATTERN,
                        Glyph::BarsPattern,
                    ),
                    DrawingToolEntry::tool(
                        Kind::Projection,
                        drawing::PROJECTION,
                        Glyph::Projection,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::VOLUME_BASED,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::AnchoredVwap,
                        drawing::ANCHORED_VWAP,
                        Glyph::AnchoredVwap,
                    ),
                    DrawingToolEntry::tool(
                        Kind::FixedRangeVolumeProfile,
                        drawing::FIXED_RANGE_VOLUME_PROFILE,
                        Glyph::FixedRangeVolumeProfile,
                    ),
                    DrawingToolEntry::tool(
                        Kind::AnchoredVolumeProfile,
                        drawing::ANCHORED_VOLUME_PROFILE,
                        Glyph::AnchoredVolumeProfile,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::MEASURERS,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::PriceRange,
                        drawing::PRICE_RANGE,
                        Glyph::PriceRange,
                    ),
                    DrawingToolEntry::tool(Kind::DateRange, drawing::DATE_RANGE, Glyph::DateRange),
                    DrawingToolEntry::tool(
                        Kind::DatePriceRange,
                        drawing::DATE_AND_PRICE_RANGE,
                        Glyph::DatePriceRange,
                    ),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_shapes",
        menu_id: "drawing_group_shapes_menu",
        label: drawing::GROUP_SHAPES,
        sections: &[
            DrawingToolSection {
                title: drawing::BRUSHES,
                tools: &[
                    DrawingToolEntry::tool(Kind::Brush, drawing::BRUSH, Glyph::Brush),
                    DrawingToolEntry::tool(
                        Kind::Highlighter,
                        drawing::HIGHLIGHTER,
                        Glyph::Highlighter,
                    ),
                ],
            },
            DrawingToolSection {
                title: drawing::SHAPES,
                tools: &[
                    DrawingToolEntry::tool(Kind::Rectangle, drawing::RECTANGLE, Glyph::Rectangle),
                    DrawingToolEntry::tool(
                        Kind::RotatedRectangle,
                        drawing::ROTATED_RECTANGLE,
                        Glyph::RotatedRectangle,
                    ),
                    DrawingToolEntry::tool(Kind::Path, drawing::PATH, Glyph::Path),
                    DrawingToolEntry::tool(Kind::Circle, drawing::CIRCLE, Glyph::Circle),
                    DrawingToolEntry::tool(Kind::Ellipse, drawing::ELLIPSE, Glyph::Ellipse),
                    DrawingToolEntry::tool(Kind::Polyline, drawing::POLYLINE, Glyph::Polyline),
                    DrawingToolEntry::tool(Kind::Triangle, drawing::TRIANGLE, Glyph::Triangle),
                    DrawingToolEntry::tool(Kind::Arc, drawing::ARC, Glyph::Arc),
                    DrawingToolEntry::tool(Kind::Curve, drawing::CURVE, Glyph::Curve),
                    DrawingToolEntry::tool(
                        Kind::DoubleCurve,
                        drawing::DOUBLE_CURVE,
                        Glyph::DoubleCurve,
                    ),
                ],
            },
        ],
    },
    DrawingToolGroup {
        id: "drawing_group_text",
        menu_id: "drawing_group_text_menu",
        label: drawing::TEXT_AND_NOTES,
        sections: &[DrawingToolSection {
            title: drawing::TEXT_AND_NOTES,
            tools: &[
                DrawingToolEntry::tool(Kind::Text, drawing::TEXT, Glyph::Text),
                DrawingToolEntry::tool(
                    Kind::AnchoredText,
                    drawing::ANCHORED_TEXT,
                    Glyph::AnchoredText,
                ),
                DrawingToolEntry::tool(Kind::Note, drawing::NOTE, Glyph::Note),
                DrawingToolEntry::tool(Kind::PriceNote, drawing::PRICE_NOTE, Glyph::PriceNote),
                DrawingToolEntry::tool(Kind::Callout, drawing::CALLOUT, Glyph::Callout),
                DrawingToolEntry::tool(Kind::Comment, drawing::COMMENT, Glyph::Comment),
                DrawingToolEntry::tool(Kind::PriceLabel, drawing::PRICE_LABEL, Glyph::PriceLabel),
                DrawingToolEntry::tool(Kind::Signpost, drawing::SIGNPOST, Glyph::Signpost),
                DrawingToolEntry::tool(Kind::FlagMark, drawing::FLAG_MARK, Glyph::FlagMark),
            ],
        }],
    },
    DrawingToolGroup {
        id: "drawing_group_markers",
        menu_id: "drawing_group_markers_menu",
        label: drawing::GROUP_MARKERS,
        sections: &[
            DrawingToolSection {
                title: drawing::STAMPS,
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
                title: drawing::ARROWS,
                tools: &[
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerUp,
                        drawing::ARROW_MARK_UP,
                        Glyph::ArrowMarkerUp,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerDown,
                        drawing::ARROW_MARK_DOWN,
                        Glyph::ArrowMarkerDown,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerLeft,
                        drawing::ARROW_MARK_LEFT,
                        Glyph::ArrowMarkerLeft,
                    ),
                    DrawingToolEntry::tool(
                        Kind::ArrowMarkerRight,
                        drawing::ARROW_MARK_RIGHT,
                        Glyph::ArrowMarkerRight,
                    ),
                ],
            },
        ],
    },
];

/// The sidebar's name for a drawing kind, so dialogs title a drawing as its tool reads.
pub(super) fn drawing_kind_label(kind: ChartDrawingKind) -> Text {
    if kind == Kind::IconStamp {
        return drawing::ICON;
    }
    DRAWING_TOOL_GROUPS
        .iter()
        .flat_map(DrawingToolGroup::entries)
        .find(|entry| entry.choice == DrawingToolChoice::Kind(kind))
        .map_or(drawing::DRAWING, |entry| entry.label)
}

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
        let favorites = &mut self.chrome.preferences_mut().drawing_favorites;
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
        let favorites = &mut self.chrome.preferences_mut().drawing_favorites;
        favorites.toolbar_visible = !favorites.toolbar_visible;
        self.commit_drawing_favorites(cx);
    }

    fn begin_drawing_favorites_move(
        &mut self,
        pointer: gpui::Point<Pixels>,
        origin: gpui::Point<Pixels>,
    ) {
        self.chrome.begin_favorites_drag(pointer - origin);
    }

    /// Follows the pointer in memory only; the position is saved once when the drag ends.
    fn move_drawing_favorites(
        &mut self,
        pointer: gpui::Point<Pixels>,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(grab) = self.chrome.favorites_grab() else {
            return;
        };
        let favorites = &mut self.chrome.preferences_mut().drawing_favorites;
        let count = favorite_entries(favorites).count();
        let toolbar = floating_toolbar_size(count, self.theme.dimensions.border_width);
        let origin = clamp_floating_toolbar_origin(pointer - grab, viewport, toolbar);
        let origin = Some((f32::from(origin.x), f32::from(origin.y)));
        if favorites.toolbar_origin != origin {
            favorites.toolbar_origin = origin;
            cx.notify();
        }
    }

    pub(super) fn end_drawing_favorites_move(&mut self, cx: &mut Context<Self>) {
        if self.chrome.end_favorites_drag() {
            self.commit_drawing_favorites(cx);
        }
    }

    /// Every surface saves its own copy of the chrome preferences, so each one must hold the
    /// current favorites before any of them writes the shared file.
    fn commit_drawing_favorites(&mut self, cx: &mut Context<Self>) {
        let favorites = self.chrome.preferences().drawing_favorites;
        for workspace in self.tabs.iter() {
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
    pub(super) drawings: chart_chrome::ChromeVisibility,
}

pub(super) fn drawing_toolbar(
    terminal: &Entity<TerminalApp>,
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
        .map(|slot| drawing_tool_slot(terminal, slot, theme));
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
                        .child(drawing_toolbar_actions(
                            terminal,
                            app,
                            state,
                            sidebar.drawings,
                            theme,
                        )),
                )
                .child(ThinScrollbar::new(
                    scroll,
                    gpui_color(colors.text_secondary),
                )),
        )
        .child(
            div()
                .flex_none()
                .w_full()
                .flex()
                .justify_center()
                .py_1()
                .border_t_1()
                .border_color(gpui_color(colors.border))
                .child(drawing_favorites_toggle(terminal, sidebar.favorites, theme)),
        )
        .children(panel_corners(
            theme,
            PanelEdges::INNER.with(PanelEdges::LEFT, false),
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
    let button = sidebar_tooltip(
        group.id,
        tr!(entry.label),
        drawing_toolbar_button(
            group.id,
            entry.icon(theme),
            tr!(entry.label),
            entry.toolbar_icon_size(),
            theme,
            slot.selected,
        )
        .disabled(!slot.enabled)
        .on_click(move |_, _, cx| {
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
                        if terminal.chrome.tool_menu_mut().track_slot(index, bounds) {
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
    let group = &DRAWING_TOOL_GROUPS[slot.group];
    let toggle = terminal.clone();
    let index = slot.group;
    // A disclosure strip under the tool button: the tool button's width, the strip's own height.
    Button::new(group.menu_id, theme)
        .variant(ButtonVariant::Ghost)
        .icon(Icon::new(Glyph::GroupArrow.path()).with_size(px(DRAWING_TOOL_GROUP_ARROW_ICON)))
        .aria_label(tr!(group.label))
        .tooltip(
            TooltipSpec::new(tr!(group.label), theme)
                .show_delay(TOOLTIP_OPEN_DELAY)
                .placement(TooltipPlacement::Right),
        )
        .selected(slot.menu_open)
        .disabled(!slot.enabled)
        .on_press(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_drawing_tool_menu(index, terminal_cx);
            });
        })
        .w(px(DRAWING_TOOL_BUTTON_SIZE))
        .h(px(DRAWING_TOOL_GROUP_ARROW_HEIGHT))
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
        drawing_tool_menu_height(group, rem_size, theme),
    );
    let origin = drawing_tool_menu_origin(trigger, panel_size, viewport);
    let mut panel = MenuPanel::new(
        ("drawing_tool_menu", index),
        MenuPlacement::At(origin),
        theme,
    )
    .width(panel_size.width)
    .max_height((viewport.height - px(2.0 * OVERLAY_EDGE_MARGIN)).max(px(0.0)));
    let mut row = 0_usize;
    for (section_index, section) in group.sections.iter().enumerate() {
        if section_index > 0 {
            panel = panel.separator();
        }
        panel = panel.child(drawing_tool_menu_title(tr!(section.title), theme));
        for entry in section.tools.iter().copied() {
            let select = terminal.clone();
            panel = panel.child(
                MenuRow::compact(("drawing_tool_menu_row", row), tr!(entry.label), theme)
                    .leading(entry.icon(theme).with_size(px(DRAWING_TOOL_MENU_ICON)))
                    .trailing(drawing_favorite_star(
                        terminal,
                        row,
                        entry.choice,
                        entry.choice.is_favorite(favorites),
                        theme,
                    ))
                    .highlighted(entry.choice == armed)
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
    let panel = panel.animate_from(PopupAnimationOrigin::from_trigger(
        trigger.center(),
        Bounds::new(origin, panel_size),
    ));
    let dismiss = terminal.clone();
    let dismiss_right = terminal.clone();
    Some(
        div()
            .id("drawing_tool_menu_scrim")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .on_click(move |_, _, cx| {
                dismiss.update(cx, |terminal, terminal_cx| {
                    terminal.close_drawing_tool_menu(terminal_cx);
                });
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                dismiss_right.update(cx, |terminal, terminal_cx| {
                    terminal.close_drawing_tool_menu(terminal_cx);
                });
                cx.stop_propagation();
            })
            .child(panel)
            .into_any_element(),
    )
}

const FLOATING_TOOLBAR_PADDING_X: f32 = 4.0;
const FLOATING_TOOLBAR_PADDING_Y: f32 = 2.0;
pub(super) const FLOATING_TOOLBAR_BUTTON_SIZE: f32 = 28.0;
pub(super) const FLOATING_TOOLBAR_GAP: f32 = 2.0;
const FLOATING_TOOLBAR_GRIP_WIDTH: f32 = 14.0;
const FLOATING_TOOLBAR_GRIP_DOT: f32 = 3.0;
/// How far inside the chart a floating toolbar opens until it is first dragged.
pub(super) const FLOATING_TOOLBAR_DEFAULT_INSET: f32 = 12.0;

/// The size of a floating toolbar of `count` buttons after its grip. The geometry is fixed, so
/// moves clamp the toolbar into the window without measuring a frame.
pub(super) fn floating_toolbar_size(count: usize, border_width: f32) -> gpui::Size<Pixels> {
    let count = f32::from(u16::try_from(count).unwrap_or(u16::MAX));
    size(
        px(2.0 * (FLOATING_TOOLBAR_PADDING_X + border_width)
            + FLOATING_TOOLBAR_GRIP_WIDTH
            + count * (FLOATING_TOOLBAR_GAP + FLOATING_TOOLBAR_BUTTON_SIZE)),
        px(2.0 * (FLOATING_TOOLBAR_PADDING_Y + border_width) + FLOATING_TOOLBAR_BUTTON_SIZE),
    )
}

/// A floating toolbar may sit flush against any window edge, for example across the bottom
/// panel's header row, so it uses no overlay edge margin.
pub(super) fn clamp_floating_toolbar_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    toolbar: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    clamp_panel_origin_within(origin, viewport, toolbar, px(0.0))
}

fn floating_toolbar_radius() -> Pixels {
    px(f32::from(RadiusToken::Compact.logical_pixels()))
}

/// The panel of a floating toolbar at `origin`. It keeps presses to itself so the chart under
/// it never sees them.
pub(super) fn floating_toolbar_panel(
    id: &'static str,
    origin: gpui::Point<Pixels>,
    panel_size: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> gpui::Stateful<Div> {
    popover_surface(div().id(id), theme, floating_toolbar_radius())
        .absolute()
        .left(origin.x)
        .top(origin.y)
        .w(panel_size.width)
        .h(panel_size.height)
        .flex()
        .items_center()
        .gap(px(FLOATING_TOOLBAR_GAP))
        .px(px(FLOATING_TOOLBAR_PADDING_X))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The dotted grip that moves a floating toolbar. `on_grab` receives the press position; the
/// toolbar follows `drag` moves until the press ends.
pub(super) fn floating_toolbar_grip<D, G>(
    id: &'static str,
    label: &'static str,
    drag: D,
    theme: &AerisTheme,
    on_grab: G,
) -> impl IntoElement + use<D, G>
where
    D: Render + Clone,
    G: Fn(gpui::Point<Pixels>, &mut App) + 'static,
{
    let dot = || {
        div()
            .size(px(FLOATING_TOOLBAR_GRIP_DOT))
            .rounded_full()
            .bg(gpui_color(theme.colors.text_muted))
    };
    let column = || {
        div()
            .flex()
            .flex_col()
            .gap(px(FLOATING_TOOLBAR_GRIP_DOT))
            .child(dot())
            .child(dot())
            .child(dot())
    };
    let spec = TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY);
    let grip = div()
        .id(id)
        .flex_none()
        .w(px(FLOATING_TOOLBAR_GRIP_WIDTH))
        .h(px(FLOATING_TOOLBAR_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(FLOATING_TOOLBAR_GRIP_DOT))
        .cursor(FLOATING_PANEL_MOVE_CURSOR)
        .role(Role::Button)
        .aria_label(label)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            on_grab(event.position, cx);
        })
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .child(column())
        .child(column());
    with_tooltip((id, usize::MAX), grip, &spec)
}

/// The default spot beside the drawing sidebar and below the chart header.
pub(super) fn drawing_favorites_default_origin(chart_top: f32) -> gpui::Point<Pixels> {
    point(
        px(chart_chrome::CHART_CHROME_HEIGHT + PANEL_GAP + FLOATING_TOOLBAR_DEFAULT_INSET),
        px(chart_top + FLOATING_TOOLBAR_DEFAULT_INSET),
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
    let panel_size = floating_toolbar_size(entries.len(), theme.dimensions.border_width);
    let stored = favorites
        .toolbar_origin
        .map_or(toolbar.default_origin, |(x, y)| point(px(x), px(y)));
    let origin = clamp_floating_toolbar_origin(stored, toolbar.viewport, panel_size);
    let buttons = entries.into_iter().enumerate().map(|(index, entry)| {
        let arm = terminal.clone();
        let selected = entry.choice == armed;
        let button = drawing_toolbar_button(
            ("drawing_favorite", index),
            entry.icon(theme),
            tr!(entry.label),
            entry.toolbar_icon_size(),
            theme,
            selected,
        )
        .button_size(ButtonSize::Default)
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            arm.update(cx, |terminal, terminal_cx| {
                terminal.select_drawing_tool_on_active_workspace(entry.choice, terminal_cx);
            });
        });
        let tooltip = TooltipSpec::new(tr!(entry.label), theme).show_delay(TOOLTIP_OPEN_DELAY);
        with_tooltip(("drawing_favorite_tooltip", index), button, &tooltip).into_any_element()
    });
    let move_terminal = terminal.clone();
    let grab = terminal.clone();
    Some(
        floating_toolbar_panel("drawing_favorites_toolbar", origin, panel_size, theme)
            .on_drag_move::<DrawingFavoritesMoveDrag>(move |event, window, cx| {
                let viewport = window.viewport_size();
                move_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.move_drawing_favorites(event.event.position, viewport, terminal_cx);
                });
            })
            .child(floating_toolbar_grip(
                "drawing_favorites_grip",
                tr!(drawing::MOVE_FAVORITES_TOOLBAR),
                DrawingFavoritesMoveDrag,
                theme,
                move |pointer, cx| {
                    grab.update(cx, |terminal, _| {
                        terminal.begin_drawing_favorites_move(pointer, origin);
                    });
                },
            ))
            .children(buttons)
            .into_any_element(),
    )
}

/// The row's own star target. It handles the press itself so starring a tool neither arms it
/// nor closes the flyout.
fn drawing_favorite_star(
    terminal: &Entity<TerminalApp>,
    row: usize,
    choice: DrawingToolChoice,
    favorite: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let label = if favorite {
        tr!(drawing::REMOVE_FROM_FAVORITES)
    } else {
        tr!(drawing::ADD_TO_FAVORITES)
    };
    let toggle = terminal.clone();
    let star = if favorite {
        // A starred tool keeps the warning-coloured star in every state.
        Icon::new(HugeIcon::StarFilled.path()).color(gpui_color(theme.colors.warning))
    } else {
        Icon::new(HugeIcon::Star.path())
    };
    Button::new(("drawing_favorite_star", row), theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .icon(star)
        .aria_label(label)
        .tooltip(TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_drawing_favorite(choice, terminal_cx);
            });
        })
}

fn drawing_tool_menu_title(title: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .h(gpui::rems(DRAWING_TOOL_MENU_TITLE_REMS))
        .flex()
        .items_center()
        // Lines the title up with the row labels inside the panel inset.
        .px(gpui::rems(0.5))
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(title)
}

fn drawing_tool_menu_height(
    group: &DrawingToolGroup,
    rem_size: Pixels,
    theme: &AerisTheme,
) -> Pixels {
    let rem = f32::from(rem_size);
    let sections = group.sections.len();
    let rows = group.entries().count();
    let count = |value: usize| f32::from(u16::try_from(value).unwrap_or(u16::MAX));
    px(count(rows) * DRAWING_TOOL_MENU_ROW_REMS * rem
        + count(sections) * DRAWING_TOOL_MENU_TITLE_REMS * rem
        + count(sections.saturating_sub(1)) * MENU_SEPARATOR_HEIGHT)
        + menu_panel_chrome_height(theme, MenuScale::BASE, rem_size)
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
        tr!(drawing::STAR_TOOL_HINT)
    } else if shown {
        tr!(drawing::HIDE_FAVORITES_TOOLBAR)
    } else {
        tr!(drawing::SHOW_FAVORITES_TOOLBAR)
    };
    let toggle = terminal.clone();
    sidebar_tooltip(
        "drawing_favorites_toggle",
        label,
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
        )
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            toggle.update(cx, TerminalApp::toggle_drawing_favorites_toolbar);
        }),
        theme,
    )
}

/// Hides or shows every drawing on every chart; arming a drawing tool shows them again.
fn drawings_visibility_toggle(
    terminal: &Entity<TerminalApp>,
    drawings: chart_chrome::ChromeVisibility,
    theme: &AerisTheme,
) -> AnyElement {
    let hidden = !drawings.shown();
    let label = if hidden {
        tr!(drawing::SHOW_ALL_DRAWINGS)
    } else {
        tr!(drawing::HIDE_ALL_DRAWINGS)
    };
    let toggle = terminal.clone();
    sidebar_tooltip(
        "drawings_visibility_toggle",
        label,
        drawing_toolbar_button(
            "drawings_visibility_toggle",
            header_icon(if hidden {
                HugeIcon::ViewOff
            } else {
                HugeIcon::View
            }),
            label,
            20.0,
            theme,
            hidden,
        )
        .on_click(move |_, _, cx| {
            toggle.update(cx, TerminalApp::toggle_drawings_hidden);
        }),
        theme,
    )
}

fn drawing_toolbar_actions(
    terminal: &Entity<TerminalApp>,
    app: &Entity<WorkspaceSurface>,
    state: DrawingToolbarState,
    drawings: chart_chrome::ChromeVisibility,
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
                drawing::DELETE_SELECTED_CHART_OBJECT,
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
                drawing::LOCK_OR_UNLOCK_SELECTED_DRAWING,
                HugeIcon::Lock,
                20.0,
                state.selected_locked,
                state.selection == DrawingToolbarSelection::Drawing,
                WorkspaceSurface::toggle_selected_drawing_lock,
            ),
            app.clone(),
            theme,
        ))
        .child(drawings_visibility_toggle(terminal, drawings, theme))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_clear_all",
                drawing::CLEAR_ALL_DRAWINGS,
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

#[derive(Clone, Copy)]
struct DrawingActionSpec {
    id: &'static str,
    tooltip: Text,
    icon: HugeIcon,
    icon_size: f32,
    selected: bool,
    enabled: bool,
    action: fn(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
}

impl DrawingActionSpec {
    const fn new(
        id: &'static str,
        tooltip: Text,
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
        tr!(spec.tooltip),
        spec.icon_size,
        theme,
        spec.selected,
    )
    .disabled(!spec.enabled)
    .on_click(move |_, _, cx| {
        app.update(cx, spec.action);
    });
    sidebar_tooltip(spec.id, tr!(spec.tooltip), button, theme)
}

/// The sidebar sits on the window's left edge, so its tooltips open to the right.
fn sidebar_tooltip(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    trigger: impl IntoElement + 'static,
    theme: &AerisTheme,
) -> AnyElement {
    let spec = TooltipSpec::new(label, theme)
        .show_delay(TOOLTIP_OPEN_DELAY)
        .placement(TooltipPlacement::Right);
    with_tooltip((id, usize::MAX), trigger, &spec).into_any_element()
}

/// A drawing-toolbar tool or action. Each glyph keeps its own optical size because the tool
/// artwork is drawn on differently padded artboards.
pub(super) fn drawing_toolbar_button(
    id: impl Into<gpui::ElementId>,
    icon: Icon,
    label: &'static str,
    icon_size: f32,
    theme: &AerisTheme,
    selected: bool,
) -> Button {
    Button::new(id, theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .icon(icon.with_size(px(icon_size)))
        .aria_label(label)
        .selected(selected)
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
    fn floating_toolbars_grow_one_button_at_a_time() {
        let one = floating_toolbar_size(1, 1.0);
        let three = floating_toolbar_size(3, 1.0);
        assert_eq!(
            one.height,
            px(2.0 * (FLOATING_TOOLBAR_PADDING_Y + 1.0) + FLOATING_TOOLBAR_BUTTON_SIZE)
        );
        assert_eq!(
            three.width - one.width,
            px(2.0 * (FLOATING_TOOLBAR_GAP + FLOATING_TOOLBAR_BUTTON_SIZE))
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
        let theme = AerisTheme::light();
        let height = drawing_tool_menu_height(lines, px(16.0), &theme);
        let rows = 20.0 * 32.0;
        let titles = 3.0 * 28.0;
        let separators = 2.0 * MENU_SEPARATOR_HEIGHT;
        let panel_chrome = 2.0 * (MENU_PANEL_INSET + theme.dimensions.border_width);
        assert_eq!(height, px(rows + titles + separators + panel_chrome));
    }
}
