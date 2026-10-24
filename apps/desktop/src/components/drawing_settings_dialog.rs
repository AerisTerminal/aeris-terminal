//! The drawing settings dialog: one form for every Aeris Charts drawing kind, built from the
//! kind's property schema. Edits preview live inside an Aeris Charts settings session; Ok keeps
//! them as one undo step and Cancel restores the drawing as it was.
//!
//! Related properties share one labelled row (the stroke, the font, paired alignments), choices
//! are dropdowns and switches are checkboxes, so every kind gets the same compact form. The
//! dialog moves anywhere in the window by its title bar.

use super::*;
use aeris_chart_integration::{
    ChartDrawingIntervalUnit, ChartDrawingIntervalVisibility, ChartDrawingLevel,
    ChartDrawingProperty, ChartDrawingPropertyGroup as Group, ChartDrawingPropertyType as Type,
    ChartDrawingSettings, MAX_DRAWING_LEVELS,
};
use chart_context_menus::{clamp_floating_panel_origin, color_swatch_dot};
use drawing_context_toolbar::{choice_glyph, width_choices, width_glyph, width_label};
use drawing_toolbar::drawing_kind_label;
use serde_json::{Value, json};

/// The dialog tabs in display order. `General` properties belong to the toolbar and object
/// tree, so the dialog never shows them.
const DIALOG_GROUPS: [(Group, &str); 5] = [
    (Group::Inputs, "Inputs"),
    (Group::Style, "Style"),
    (Group::Text, "Text"),
    (Group::Coordinates, "Coordinates"),
    (Group::Visibility, "Visibility"),
];

/// The interval units the Visibility tab offers and the span each unit's range may cover.
const INTERVAL_UNITS: [(ChartDrawingIntervalUnit, &str, f64, f64); 7] = [
    (ChartDrawingIntervalUnit::Ticks, "Ticks", 1.0, 1000.0),
    (ChartDrawingIntervalUnit::Seconds, "Seconds", 1.0, 59.0),
    (ChartDrawingIntervalUnit::Minutes, "Minutes", 1.0, 59.0),
    (ChartDrawingIntervalUnit::Hours, "Hours", 1.0, 24.0),
    (ChartDrawingIntervalUnit::Days, "Days", 1.0, 366.0),
    (ChartDrawingIntervalUnit::Weeks, "Weeks", 1.0, 52.0),
    (ChartDrawingIntervalUnit::Months, "Months", 1.0, 12.0),
];

/// The font sizes the font menu offers, within the property's own bounds.
const FONT_SIZES: [f64; 10] = [10.0, 11.0, 12.0, 14.0, 16.0, 20.0, 24.0, 28.0, 32.0, 40.0];
/// The stroke row: its color, width and style.
const LINE_PARTS: [&str; 3] = ["color", "width", "style"];
/// The font row that opens the Text tab.
const FONT_PARTS: [&str; 4] = ["text_color", "text_size", "text_bold", "text_italic"];
/// Two choices that read as one setting, shown side by side under one label.
const PAIRED_CHOICES: [(&str, &str, &str); 3] = [
    ("stroke_start", "stroke_end", "Line ends"),
    ("text_v_align", "text_h_align", "Text alignment"),
    ("level_label_align", "level_label_v_align", "Labels"),
];
/// A checkbox and the color it turns on, shown on one row.
const TOGGLED_COLORS: [(&str, &str); 1] = [("fill_enabled", "fill_color")];
/// A checkbox and the stroke it turns on (color, width and style), shown on one row.
const TOGGLED_LINES: [(&str, [&str; 3]); 1] = [(
    "middle_line_visible",
    [
        "middle_line_color",
        "middle_line_width",
        "middle_line_style",
    ],
)];
/// The Extend choices over `extend_left` and `extend_right`.
const EXTEND_CHOICES: [(&str, bool, bool); 4] = [
    ("Don't extend", false, false),
    ("Left", true, false),
    ("Right", false, true),
    ("Both", true, true),
];
/// The menu key of the combined Extend choice; every other menu is keyed by its property.
const EXTEND_MENU: &str = "extend";

const LABEL_WIDTH: f32 = 112.0;
const SELECT_WIDTH: f32 = 128.0;
const COMPACT_SELECT_WIDTH: f32 = 84.0;
const NUMBER_FIELD_WIDTH: f32 = 96.0;
const LEVEL_FIELD_WIDTH: f32 = 72.0;
const INTERVAL_FIELD_WIDTH: f32 = 64.0;
const FIELD_HEIGHT: f32 = 28.0;
const TEXT_BODY_HEIGHT: f32 = 96.0;
const DIALOG_MAX_HEIGHT: f32 = 640.0;

/// An open drawing settings dialog. The drawing itself is the draft: every control writes to
/// Aeris Charts inside the session `chart` began, and the form reads the drawing back.
pub(super) struct DrawingSettingsDialogState {
    chart: Entity<AerisChartView>,
    drawing: ChartDrawingId,
    title: &'static str,
    tab: Group,
    /// Held by the panel while none of its fields has focus, so keys never reach the chart.
    focus: FocusHandle,
    /// Window position once the title bar has dragged it; centered over the pane until then.
    origin: Option<gpui::Point<Pixels>>,
    /// Pointer offset from the panel corner captured when a title-bar drag begins.
    grab: gpui::Point<Pixels>,
    /// The dropdown that is open: a property name or [`EXTEND_MENU`].
    menu: Option<String>,
    /// Text fields of number and string properties, by property name.
    inputs: HashMap<String, Entity<InputState>>,
    /// The drawing's own text, when its kind carries one.
    text_body: Option<Entity<TextareaState>>,
    /// Value fields of each levels property's rows, by property name.
    levels: HashMap<String, Vec<Entity<InputState>>>,
    anchors: Vec<AnchorInputs>,
    intervals: Vec<IntervalInputs>,
    color_picker: Option<DrawingColorPicker>,
    _subscriptions: Vec<gpui::Subscription>,
    level_subscriptions: HashMap<String, Vec<gpui::Subscription>>,
}

struct AnchorInputs {
    index: usize,
    label: String,
    show_price: bool,
    price: Entity<InputState>,
    time: Option<Entity<InputState>>,
    error: Option<String>,
}

struct IntervalInputs {
    min: Entity<InputState>,
    max: Entity<InputState>,
}

/// The color a dialog swatch edits: a color property or one level row's color.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DrawingColorTarget {
    Property(String),
    Level { property: String, index: usize },
}

struct DrawingColorPicker {
    target: DrawingColorTarget,
    input: Entity<InputState>,
    error: Option<String>,
}

/// The field a text input edits.
#[derive(Clone, Debug)]
enum DrawingInputTarget {
    Property(String),
    Level {
        property: String,
        index: usize,
    },
    /// Anchor coordinates apply when the field is submitted or loses focus, since a partly
    /// typed time is not a time.
    Anchor(usize),
    /// Any interval range field; the whole visibility set is rebuilt from every row.
    Interval,
}

/// Numbers chosen from a menu rather than typed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NumberMenu {
    Width,
    FontSize,
}

fn number_menu(property: &ChartDrawingProperty) -> Option<NumberMenu> {
    if !matches!(property.property_type, Type::Number | Type::Integer) {
        return None;
    }
    if property.name == "text_size" {
        Some(NumberMenu::FontSize)
    } else if property.name.ends_with("width") {
        Some(NumberMenu::Width)
    } else {
        None
    }
}

/// One row of a tab, in schema order. Grouped rows take the place of their first member.
#[derive(Debug, PartialEq)]
enum Row<'a> {
    /// The stroke's color, width and style.
    Line(Vec<&'a ChartDrawingProperty>),
    /// `extend_left` and `extend_right` as one choice.
    Extend,
    /// The text color, size, weight and slant.
    Font(Vec<&'a ChartDrawingProperty>),
    /// The drawing's own text.
    TextBody,
    /// Two choices under one label.
    Pair(
        &'static str,
        &'a ChartDrawingProperty,
        &'a ChartDrawingProperty,
    ),
    /// A checkbox and the color it turns on.
    ToggledColor(&'a ChartDrawingProperty, &'a ChartDrawingProperty),
    /// A checkbox and the stroke color, width and style it turns on.
    ToggledLine(&'a ChartDrawingProperty, Vec<&'a ChartDrawingProperty>),
    Property(&'a ChartDrawingProperty),
}

/// The rows of one tab: the tab's properties in schema order, with related ones merged.
fn dialog_rows(properties: &[ChartDrawingProperty], tab: Group) -> Vec<Row<'_>> {
    let properties: Vec<&ChartDrawingProperty> = properties
        .iter()
        .filter(|property| property.group == tab)
        .collect();
    let find = |name: &str| {
        properties
            .iter()
            .copied()
            .find(|property| property.name == name)
    };
    let members = |names: &[&str]| -> Vec<&ChartDrawingProperty> {
        names.iter().filter_map(|name| find(name)).collect()
    };
    let mut grouped: Vec<&str> = Vec::new();
    let mut rows = Vec::new();
    for property in &properties {
        let name = property.name.as_str();
        if grouped.contains(&name) {
            continue;
        }
        let pair = PAIRED_CHOICES
            .iter()
            .find(|(first, second, _)| name == *first || name == *second)
            .and_then(|&(first, second, label)| Some((label, find(first)?, find(second)?)));
        let toggled = TOGGLED_COLORS
            .iter()
            .find(|(flag, color)| name == *flag || name == *color)
            .and_then(|&(flag, color)| Some((find(flag)?, find(color)?)));
        let toggled_line = TOGGLED_LINES
            .iter()
            .find(|(flag, parts)| name == *flag || parts.contains(&name))
            .and_then(|(flag, parts)| {
                let parts = members(parts);
                (!parts.is_empty()).then_some((find(flag)?, parts))
            });
        let (row, taken): (Row<'_>, Vec<&str>) =
            if LINE_PARTS.contains(&name) && members(&LINE_PARTS).len() > 1 {
                (Row::Line(members(&LINE_PARTS)), LINE_PARTS.to_vec())
            } else if FONT_PARTS.contains(&name) {
                (Row::Font(members(&FONT_PARTS)), FONT_PARTS.to_vec())
            } else if matches!(name, "extend_left" | "extend_right")
                && find("extend_left").is_some()
                && find("extend_right").is_some()
            {
                (Row::Extend, vec!["extend_left", "extend_right"])
            } else if let Some((label, first, second)) = pair {
                (
                    Row::Pair(label, first, second),
                    vec![first.name.as_str(), second.name.as_str()],
                )
            } else if let Some((flag, color)) = toggled {
                (
                    Row::ToggledColor(flag, color),
                    vec![flag.name.as_str(), color.name.as_str()],
                )
            } else if let Some((flag, parts)) = toggled_line {
                let taken = std::iter::once(flag.name.as_str())
                    .chain(parts.iter().map(|part| part.name.as_str()))
                    .collect();
                (Row::ToggledLine(flag, parts), taken)
            } else if name == "text" && property.property_type == Type::String {
                (Row::TextBody, vec![name])
            } else {
                (Row::Property(property), vec![name])
            };
        grouped.extend(taken);
        rows.push(row);
    }
    rows
}

fn group_has_content(settings: &ChartDrawingSettings, group: Group) -> bool {
    (group == Group::Coordinates && !settings.schema.anchors.is_empty())
        || settings
            .schema
            .properties
            .iter()
            .any(|property| property.group == group)
}

/// `snake_case` engine identifiers as sentence-case labels.
pub(super) fn humanize(identifier: &str) -> String {
    let text = identifier.replace('_', " ");
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn number_text(value: f64) -> String {
    value.to_string()
}

/// A typed number inside the property's bounds; integers must be whole.
fn bounded_number(text: &str, property: &ChartDrawingProperty) -> Option<f64> {
    let value = text
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    let in_bounds =
        property.min.is_none_or(|min| value >= min) && property.max.is_none_or(|max| value <= max);
    let whole = property.property_type != Type::Integer || value.fract() == 0.0;
    (in_bounds && whole).then_some(value)
}

fn font_size_choices(property: &ChartDrawingProperty) -> impl Iterator<Item = f64> + '_ {
    FONT_SIZES.into_iter().filter(|size| {
        property.min.is_none_or(|min| *size >= min) && property.max.is_none_or(|max| *size <= max)
    })
}

/// Drawing text is one line, as the chart's own text editor keeps it: line breaks and other
/// control characters become spaces.
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn unit_checked(
    visibility: &ChartDrawingIntervalVisibility,
    unit: ChartDrawingIntervalUnit,
) -> bool {
    !visibility.enabled || visibility.ranges.iter().any(|range| range.unit == unit)
}

fn unit_range(
    visibility: &ChartDrawingIntervalVisibility,
    unit: ChartDrawingIntervalUnit,
    span: (f64, f64),
) -> (f64, f64) {
    visibility
        .ranges
        .iter()
        .find(|range| range.unit == unit)
        .map_or(span, |range| (range.min, range.max))
}

fn text_input(
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> Entity<InputState> {
    let text = text.into();
    let input = cx.new(|input_cx| InputState::new(window, input_cx));
    input.update(cx, |input, input_cx| {
        input.set_value(text, window, input_cx);
    });
    input
}

fn watch_input(
    input: &Entity<InputState>,
    target: DrawingInputTarget,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> gpui::Subscription {
    let surface = cx.entity().downgrade();
    window.subscribe(input, cx, move |_, event: &InputEvent, _, cx| {
        if let Err(error) = surface.update(cx, |surface, surface_cx| {
            surface.drawing_settings_input_event(&target, event, surface_cx);
        }) && surface.upgrade().is_some()
        {
            diagnostic!("Live pane could not receive drawing settings input: {error}");
        }
    })
}

/// The drawing's text field. Enter does not break the line, since drawing text is one line.
fn text_body_input(
    text: &str,
    subscriptions: &mut Vec<gpui::Subscription>,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> Entity<TextareaState> {
    let input = cx.new(|input_cx| {
        TextareaState::new(window, input_cx)
            .submit_on_enter(true)
            .placeholder("Add text")
    });
    input.update(cx, |input, input_cx| {
        input.set_value(text.to_string(), window, input_cx);
    });
    let surface = cx.entity().downgrade();
    subscriptions.push(window.subscribe(
        &input,
        cx,
        move |input, event: &InputEvent, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let typed = input.read(cx).value().to_string();
            let text = single_line(&typed);
            if text != typed {
                input.update(cx, |input, input_cx| {
                    input.set_value(text.clone(), window, input_cx);
                });
            }
            if let Err(error) = surface.update(cx, |surface, surface_cx| {
                surface.set_drawing_settings_value("text", Value::String(text), surface_cx);
            }) && surface.upgrade().is_some()
            {
                diagnostic!("Live pane could not receive drawing text input: {error}");
            }
        },
    ));
    input
}

/// Commits or cancels the Aeris Charts session a dialog began.
fn end_drawing_settings_session(dialog: &DrawingSettingsDialogState, keep: bool, cx: &mut App) {
    dialog.chart.update(cx, |chart, chart_cx| {
        let ended = if keep {
            chart.commit_drawing_settings_edit()
        } else {
            chart.cancel_drawing_settings_edit()
        };
        if ended {
            chart_cx.notify();
        }
    });
}

impl DrawingSettingsDialogState {
    fn new(
        chart: Entity<AerisChartView>,
        settings: &ChartDrawingSettings,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let mut inputs = HashMap::new();
        let mut text_body = None;
        let mut levels = HashMap::new();
        let mut level_subscriptions = HashMap::new();
        for property in settings
            .schema
            .properties
            .iter()
            .filter(|property| property.group != Group::General)
        {
            let name = property.name.clone();
            let text = match property.property_type {
                Type::Number | Type::Integer if number_menu(property).is_none() => {
                    settings.number(&name).map(number_text).unwrap_or_default()
                }
                Type::String if name == "text" => {
                    text_body = Some(text_body_input(
                        settings.text(&name),
                        &mut subscriptions,
                        window,
                        cx,
                    ));
                    continue;
                }
                Type::String => settings.text(&name).to_string(),
                Type::Levels => {
                    let (rows, watching) = level_inputs(&name, &settings.levels(&name), window, cx);
                    levels.insert(name.clone(), rows);
                    level_subscriptions.insert(name, watching);
                    continue;
                }
                _ => continue,
            };
            let input = text_input(text, window, cx);
            subscriptions.push(watch_input(
                &input,
                DrawingInputTarget::Property(name.clone()),
                window,
                cx,
            ));
            inputs.insert(name, input);
        }
        let anchors = anchor_inputs(&chart, settings, &mut subscriptions, window, cx);
        let intervals = interval_inputs(settings, &mut subscriptions, window, cx);
        let tab = DIALOG_GROUPS
            .iter()
            .map(|&(group, _)| group)
            .find(|&group| group_has_content(settings, group))
            .unwrap_or(Group::Visibility);
        Self {
            chart,
            drawing: settings.id,
            title: drawing_kind_label(settings.kind()),
            tab,
            focus: cx.focus_handle(),
            origin: None,
            grab: gpui::Point::default(),
            menu: None,
            inputs,
            text_body,
            levels,
            anchors,
            intervals,
            color_picker: None,
            _subscriptions: subscriptions,
            level_subscriptions,
        }
    }
}

/// The Coordinates fields: each editable anchor's price and, when the kind places it in time,
/// its time.
fn anchor_inputs(
    chart: &Entity<AerisChartView>,
    settings: &ChartDrawingSettings,
    subscriptions: &mut Vec<gpui::Subscription>,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> Vec<AnchorInputs> {
    let mut anchors = Vec::new();
    for (slot, descriptor) in settings.schema.anchors.iter().enumerate() {
        let Some(text) = chart
            .read(cx)
            .drawing_anchor_text(settings.id, descriptor.index)
        else {
            continue;
        };
        let price = text_input(text.price, window, cx);
        subscriptions.push(watch_input(
            &price,
            DrawingInputTarget::Anchor(slot),
            window,
            cx,
        ));
        let time = text.time.filter(|_| descriptor.time).map(|time| {
            let input = text_input(time, window, cx);
            subscriptions.push(watch_input(
                &input,
                DrawingInputTarget::Anchor(slot),
                window,
                cx,
            ));
            input
        });
        anchors.push(AnchorInputs {
            index: descriptor.index,
            label: descriptor.label.clone(),
            show_price: descriptor.price,
            price,
            time,
            error: None,
        });
    }
    anchors
}

/// The Visibility tab's range fields, one row per [`INTERVAL_UNITS`] unit.
fn interval_inputs(
    settings: &ChartDrawingSettings,
    subscriptions: &mut Vec<gpui::Subscription>,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> Vec<IntervalInputs> {
    let visibility = settings.interval_visibility();
    INTERVAL_UNITS
        .iter()
        .map(|&(unit, _, min, max)| {
            let (min, max) = unit_range(&visibility, unit, (min, max));
            let min = text_input(number_text(min), window, cx);
            let max = text_input(number_text(max), window, cx);
            for input in [&min, &max] {
                subscriptions.push(watch_input(input, DrawingInputTarget::Interval, window, cx));
            }
            IntervalInputs { min, max }
        })
        .collect()
}

fn level_inputs(
    property: &str,
    levels: &[ChartDrawingLevel],
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> (Vec<Entity<InputState>>, Vec<gpui::Subscription>) {
    let mut subscriptions = Vec::with_capacity(levels.len());
    let inputs = levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            let input = text_input(number_text(level.value), window, cx);
            subscriptions.push(watch_input(
                &input,
                DrawingInputTarget::Level {
                    property: property.to_string(),
                    index,
                },
                window,
                cx,
            ));
            input
        })
        .collect();
    (inputs, subscriptions)
}

impl WorkspaceSurface {
    fn drawing_settings_dialog(&self) -> Option<&DrawingSettingsDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Drawing(dialog)) => Some(dialog),
            _ => None,
        }
    }

    fn drawing_settings_dialog_mut(&mut self) -> Option<&mut DrawingSettingsDialogState> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Drawing(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(super) fn drawing_settings_dialog_open(&self) -> bool {
        self.drawing_settings_dialog().is_some()
    }

    /// The edited drawing as it is now, inside the dialog's session.
    fn drawing_settings_dialog_settings(&self, cx: &App) -> Option<ChartDrawingSettings> {
        let dialog = self.drawing_settings_dialog()?;
        dialog.chart.read(cx).drawing_settings(dialog.drawing)
    }

    /// Opens the settings of one drawing on this pane's chart and starts the session that
    /// previews its edits.
    pub(super) fn open_drawing_settings_dialog(
        &mut self,
        drawing: ChartDrawingId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chart) = self.chart.clone() else {
            return;
        };
        let Some(settings) = chart.read(cx).drawing_settings(drawing) else {
            return;
        };
        self.close_drawing_settings_dialog(false, cx);
        let began = chart.update(cx, |chart, chart_cx| {
            // A session the drawing toolbar's color picker left open is kept as its own undo
            // step, so Cancel here restores only what this dialog changed.
            chart.commit_drawing_settings_edit();
            let began = chart.begin_drawing_settings_edit(drawing);
            chart_cx.notify();
            began
        });
        if !began {
            Self::post_indicator_failure(
                "drawing",
                "Drawing unavailable",
                "This drawing cannot be edited right now.",
                cx,
            );
            cx.notify();
            return;
        }
        let dialog = DrawingSettingsDialogState::new(chart, &settings, window, cx);
        // The chart keeps keyboard focus otherwise, and Delete or an arrow key would edit the
        // drawing underneath the open dialog.
        dialog.focus.focus(window, cx);
        self.open_indicator_settings_dialog(IndicatorSettingsDialog::Drawing(Box::new(dialog)), cx);
    }

    /// Closes the drawing dialog, keeping its edits as one undo step or restoring the drawing.
    pub(super) fn close_drawing_settings_dialog(&mut self, keep: bool, cx: &mut Context<Self>) {
        if !self.drawing_settings_dialog_open() {
            return;
        }
        if let Some(IndicatorSettingsDialog::Drawing(dialog)) =
            self.indicator_settings_dialog.take()
        {
            end_drawing_settings_session(&dialog, keep, cx);
        }
        cx.notify();
    }

    /// Cancel, the close control and Escape: restores the drawing and gives the keyboard back
    /// to the chart.
    fn cancel_drawing_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_drawing_settings_dialog(false, cx);
        self.focus_keyboard(window, cx);
    }

    /// Escape closes the open dropdown or color picker first, then the dialog.
    pub(super) fn escape_drawing_settings_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.drawing_settings_dialog_mut() else {
            return;
        };
        if dialog.menu.take().is_some() || dialog.color_picker.take().is_some() {
            cx.notify();
            return;
        }
        self.cancel_drawing_settings_dialog(window, cx);
    }

    /// Closes the dialog once its drawing is gone or its chart was replaced.
    pub(super) fn close_vanished_drawing_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.drawing_settings_dialog() else {
            return;
        };
        let current = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.entity_id() == dialog.chart.entity_id());
        if current && dialog.chart.read(cx).has_drawing(dialog.drawing) {
            return;
        }
        self.close_drawing_settings_dialog(false, cx);
    }

    /// Ok: applies typed coordinates still pending in their fields, then keeps every edit.
    fn confirm_drawing_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let anchors = self
            .drawing_settings_dialog()
            .map_or(0, |dialog| dialog.anchors.len());
        for slot in 0..anchors {
            if !self.apply_drawing_anchor_input(slot, cx) {
                if let Some(dialog) = self.drawing_settings_dialog_mut() {
                    dialog.tab = Group::Coordinates;
                }
                cx.notify();
                return;
            }
        }
        self.close_drawing_settings_dialog(true, cx);
        self.focus_keyboard(window, cx);
    }

    fn move_drawing_settings_dialog(&mut self, step: DialogMove, cx: &mut Context<Self>) {
        let Some(dialog) = self.drawing_settings_dialog_mut() else {
            return;
        };
        match step {
            DialogMove::Grab(offset) => dialog.grab = offset,
            DialogMove::Drag {
                pointer,
                panel,
                viewport,
            } => {
                let origin = clamp_floating_panel_origin(pointer - dialog.grab, viewport, panel);
                if dialog.origin != Some(origin) {
                    dialog.origin = Some(origin);
                    cx.notify();
                }
            }
        }
    }

    fn select_drawing_settings_tab(&mut self, tab: Group, cx: &mut Context<Self>) {
        if let Some(dialog) = self.drawing_settings_dialog_mut()
            && dialog.tab != tab
        {
            dialog.tab = tab;
            dialog.menu = None;
            dialog.color_picker = None;
            cx.notify();
        }
    }

    fn toggle_drawing_settings_menu(&mut self, menu: &str, cx: &mut Context<Self>) {
        if let Some(dialog) = self.drawing_settings_dialog_mut() {
            dialog.menu = if dialog.menu.as_deref() == Some(menu) {
                None
            } else {
                Some(menu.to_string())
            };
            dialog.color_picker = None;
            cx.notify();
        }
    }

    fn close_drawing_settings_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.drawing_settings_dialog_mut()
            && dialog.menu.take().is_some()
        {
            cx.notify();
        }
    }

    /// A dropdown choice: applies its patch and closes the dropdown.
    fn choose_drawing_settings_option(&mut self, patch: &Value, cx: &mut Context<Self>) {
        self.apply_drawing_settings_patch(patch, cx);
        self.close_drawing_settings_menu(cx);
    }

    fn apply_drawing_settings_patch(&mut self, patch: &Value, cx: &mut Context<Self>) {
        let Some(dialog) = self.drawing_settings_dialog() else {
            return;
        };
        let drawing = dialog.drawing;
        dialog.chart.update(cx, |chart, chart_cx| {
            if chart.apply_drawing_options(drawing, patch) {
                chart_cx.notify();
            }
        });
        cx.notify();
    }

    fn set_drawing_settings_value(&mut self, name: &str, value: Value, cx: &mut Context<Self>) {
        let mut patch = serde_json::Map::new();
        patch.insert(name.to_string(), value);
        self.apply_drawing_settings_patch(&Value::Object(patch), cx);
    }

    fn drawing_settings_input_event(
        &mut self,
        target: &DrawingInputTarget,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match (target, event) {
            (
                DrawingInputTarget::Anchor(slot),
                InputEvent::PressEnter { .. } | InputEvent::Blur,
            ) => {
                self.apply_drawing_anchor_input(*slot, cx);
                cx.notify();
            }
            (DrawingInputTarget::Property(name), InputEvent::Change) => {
                self.apply_drawing_property_input(name, cx);
            }
            (DrawingInputTarget::Level { property, index }, InputEvent::Change) => {
                self.apply_drawing_level_input(property, *index, cx);
            }
            (DrawingInputTarget::Interval, InputEvent::Change) => {
                self.apply_drawing_interval_visibility(None, cx);
            }
            _ => {}
        }
    }

    fn apply_drawing_property_input(&mut self, name: &str, cx: &mut Context<Self>) {
        let (Some(settings), Some(dialog)) = (
            self.drawing_settings_dialog_settings(cx),
            self.drawing_settings_dialog(),
        ) else {
            return;
        };
        let (Some(property), Some(input)) = (
            settings
                .schema
                .properties
                .iter()
                .find(|property| property.name == name),
            dialog.inputs.get(name),
        ) else {
            return;
        };
        let text = input.read(cx).value().to_string();
        let value = match property.property_type {
            Type::String => Value::String(text),
            Type::Number | Type::Integer => match bounded_number(&text, property) {
                Some(value) => json!(value),
                None => return,
            },
            _ => return,
        };
        self.set_drawing_settings_value(name, value, cx);
    }

    fn apply_drawing_level_input(&mut self, property: &str, index: usize, cx: &mut Context<Self>) {
        let (Some(settings), Some(dialog)) = (
            self.drawing_settings_dialog_settings(cx),
            self.drawing_settings_dialog(),
        ) else {
            return;
        };
        let Some(input) = dialog.levels.get(property).and_then(|rows| rows.get(index)) else {
            return;
        };
        let Some(value) = input
            .read(cx)
            .value()
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
        else {
            return;
        };
        let mut levels = settings.levels(property);
        let Some(level) = levels.get_mut(index) else {
            return;
        };
        level.value = value;
        self.set_drawing_levels(property, &levels, cx);
    }

    fn set_drawing_levels(
        &mut self,
        property: &str,
        levels: &[ChartDrawingLevel],
        cx: &mut Context<Self>,
    ) {
        match serde_json::to_value(levels) {
            Ok(value) => self.set_drawing_settings_value(property, value, cx),
            Err(error) => diagnostic!("Aeris drawing levels could not be encoded: {error}"),
        }
    }

    fn set_drawing_level_visible(
        &mut self,
        property: &str,
        index: usize,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.drawing_settings_dialog_settings(cx) else {
            return;
        };
        let mut levels = settings.levels(property);
        if let Some(level) = levels.get_mut(index) {
            level.visible = visible;
            self.set_drawing_levels(property, &levels, cx);
        }
    }

    /// Adds a level after the last, styled like it, and rebuilds the rows' fields.
    fn add_drawing_level(&mut self, property: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = self.drawing_settings_dialog_settings(cx) else {
            return;
        };
        let mut levels = settings.levels(property);
        if levels.len() >= MAX_DRAWING_LEVELS {
            return;
        }
        let level = levels.last().map_or_else(
            || ChartDrawingLevel {
                value: 0.0,
                color: settings.text("color").to_string(),
                visible: true,
                style: settings.text("style").to_string(),
                fill_between: false,
                fill_color: None,
                label_visible: true,
            },
            |last| ChartDrawingLevel {
                value: last.value + 1.0,
                ..last.clone()
            },
        );
        levels.push(level);
        self.set_drawing_levels(property, &levels, cx);
        self.rebuild_drawing_level_inputs(property, window, cx);
    }

    fn remove_drawing_level(
        &mut self,
        property: &str,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.drawing_settings_dialog_settings(cx) else {
            return;
        };
        let mut levels = settings.levels(property);
        if index >= levels.len() {
            return;
        }
        levels.remove(index);
        self.set_drawing_levels(property, &levels, cx);
        self.rebuild_drawing_level_inputs(property, window, cx);
    }

    fn rebuild_drawing_level_inputs(
        &mut self,
        property: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.drawing_settings_dialog_settings(cx) else {
            return;
        };
        let (rows, watching) = level_inputs(property, &settings.levels(property), window, cx);
        if let Some(dialog) = self.drawing_settings_dialog_mut() {
            dialog.levels.insert(property.to_string(), rows);
            dialog
                .level_subscriptions
                .insert(property.to_string(), watching);
            dialog.color_picker = None;
        }
        cx.notify();
    }

    /// Shows or hides one statistic of the kind's stats card.
    fn set_drawing_label_visible(&mut self, metric: &str, visible: bool, cx: &mut Context<Self>) {
        let Some(settings) = self.drawing_settings_dialog_settings(cx) else {
            return;
        };
        let mut labels = settings
            .value("labels")
            .as_array()
            .cloned()
            .unwrap_or_default();
        match labels
            .iter_mut()
            .find(|label| label.get("metric").and_then(Value::as_str) == Some(metric))
        {
            Some(label) => label["visible"] = Value::Bool(visible),
            None => labels.push(json!({
                "metric": metric,
                "visible": visible,
                "position": "on",
                "text": null,
            })),
        }
        self.set_drawing_settings_value("labels", Value::Array(labels), cx);
    }

    /// Rewrites the interval visibility from the unit checkboxes and range fields. Every unit
    /// on its full span means every interval, which Aeris Charts stores as disabled.
    fn apply_drawing_interval_visibility(
        &mut self,
        toggled: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let (Some(settings), Some(dialog)) = (
            self.drawing_settings_dialog_settings(cx),
            self.drawing_settings_dialog(),
        ) else {
            return;
        };
        let current = settings.interval_visibility();
        let mut ranges = Vec::new();
        let mut every_interval = true;
        for (slot, &(unit, _, span_min, span_max)) in INTERVAL_UNITS.iter().enumerate() {
            let checked = unit_checked(&current, unit) != (toggled == Some(slot));
            if !checked {
                every_interval = false;
                continue;
            }
            let (stored_min, stored_max) = unit_range(&current, unit, (span_min, span_max));
            let typed = |input: &Entity<InputState>| {
                input
                    .read(cx)
                    .value()
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| (span_min..=span_max).contains(value))
            };
            let inputs = &dialog.intervals[slot];
            let (min, max) = match (typed(&inputs.min), typed(&inputs.max)) {
                (Some(min), Some(max)) if min <= max => (min, max),
                _ => (stored_min, stored_max),
            };
            if (min, max) != (span_min, span_max) {
                every_interval = false;
            }
            ranges.push(json!({ "unit": unit, "min": min, "max": max }));
        }
        if every_interval {
            ranges.clear();
        }
        let visibility = json!({
            "enabled": !every_interval,
            "intervals": current.intervals,
            "ranges": ranges,
        });
        self.set_drawing_settings_value("interval_visibility", visibility, cx);
    }

    /// Moves one anchor to its typed coordinates, returning whether they are applied.
    fn apply_drawing_anchor_input(&mut self, slot: usize, cx: &mut Context<Self>) -> bool {
        let Some(dialog) = self.drawing_settings_dialog() else {
            return false;
        };
        let Some(anchor) = dialog.anchors.get(slot) else {
            return false;
        };
        let chart = dialog.chart.clone();
        let drawing = dialog.drawing;
        let index = anchor.index;
        let price = anchor.price.read(cx).value().to_string();
        let time = anchor
            .time
            .as_ref()
            .map(|input| input.read(cx).value().to_string());
        let unchanged = chart
            .read(cx)
            .drawing_anchor_text(drawing, index)
            .is_some_and(|current| {
                current.price.trim() == price.trim()
                    && current.time.as_deref().map(str::trim) == time.as_deref().map(str::trim)
            });
        let result = if unchanged {
            Ok(())
        } else {
            chart.update(cx, |chart, chart_cx| {
                let result = chart.set_drawing_anchor_text(drawing, index, &price, time.as_deref());
                if result.is_ok() {
                    chart_cx.notify();
                }
                result
            })
        };
        if let Some(anchor) = self
            .drawing_settings_dialog_mut()
            .and_then(|dialog| dialog.anchors.get_mut(slot))
        {
            anchor.error = result.err().map(|error| error.to_string());
        }
        result.is_ok()
    }

    fn toggle_drawing_settings_color_picker(
        &mut self,
        target: DrawingColorTarget,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.drawing_settings_dialog_mut() else {
            return;
        };
        dialog.menu = None;
        if dialog
            .color_picker
            .as_ref()
            .is_some_and(|picker| picker.target == target)
        {
            dialog.color_picker = None;
        } else {
            let text = current.unwrap_or_default().to_ascii_uppercase();
            let input = text_input(text, window, cx);
            if let Some(dialog) = self.drawing_settings_dialog_mut() {
                dialog.color_picker = Some(DrawingColorPicker {
                    target,
                    input,
                    error: None,
                });
            }
        }
        cx.notify();
    }

    fn close_drawing_settings_color_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.drawing_settings_dialog_mut()
            && dialog.color_picker.take().is_some()
        {
            cx.notify();
        }
    }

    fn set_drawing_settings_color(
        &mut self,
        target: &DrawingColorTarget,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let normalized = normalize_hex_color(color);
        if let Some(picker) = self
            .drawing_settings_dialog_mut()
            .and_then(|dialog| dialog.color_picker.as_mut())
            .filter(|picker| &picker.target == target)
        {
            picker.error = normalized
                .is_none()
                .then(|| "Enter a hex color such as #2962FF or #2962FF80.".to_string());
        }
        let Some(color) = normalized else {
            cx.notify();
            return;
        };
        match target {
            DrawingColorTarget::Property(name) => {
                self.set_drawing_settings_value(name, Value::String(color), cx);
            }
            DrawingColorTarget::Level { property, index } => {
                let Some(settings) = self.drawing_settings_dialog_settings(cx) else {
                    return;
                };
                let mut levels = settings.levels(property);
                if let Some(level) = levels.get_mut(*index) {
                    level.color = color;
                    self.set_drawing_levels(property, &levels, cx);
                }
            }
        }
    }
}

/// What every control of the open dialog reads.
#[derive(Clone, Copy)]
struct DialogUi<'a> {
    surface: &'a Entity<WorkspaceSurface>,
    dialog: &'a DrawingSettingsDialogState,
    settings: &'a ChartDrawingSettings,
    theme: &'a AerisTheme,
}

impl DialogUi<'_> {
    fn menu_open(&self, menu: &str) -> bool {
        self.dialog.menu.as_deref() == Some(menu)
    }

    /// A stable element id for the control of `property` within its kind's schema.
    fn slot(&self, property: &ChartDrawingProperty) -> usize {
        self.settings
            .schema
            .properties
            .iter()
            .position(|candidate| candidate.name == property.name)
            .unwrap_or_default()
    }
}

/// A labelled row: the label in a fixed column so every control starts on one edge.
fn field_row(label: impl Into<SharedString>, control: impl IntoElement, theme: &AerisTheme) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_h(px(FIELD_HEIGHT + 4.0))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(label.into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(control),
        )
}

fn controls() -> Div {
    div().flex().items_center().gap_2()
}

fn section_title(title: impl Into<SharedString>, theme: &AerisTheme) -> Div {
    div()
        .pt_2()
        .text_xs()
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .text_color(gpui_color(theme.colors.text_muted))
        .child(title.into().to_uppercase())
}

fn sized(width: f32, control: impl IntoElement) -> Div {
    div()
        .w(px(width))
        .h(px(FIELD_HEIGHT))
        .flex_none()
        .flex()
        .child(control)
}

fn input_control(input: &Entity<InputState>, width: f32, theme: &AerisTheme) -> Div {
    sized(width, Input::new(input).platform(theme).flex_1())
}

/// One dropdown choice and the options patch it applies.
struct SelectOption {
    label: String,
    glyph: Option<AnyElement>,
    checked: bool,
    patch: Value,
}

fn single_option(name: &str, value: Value) -> Value {
    let mut patch = serde_json::Map::new();
    patch.insert(name.to_string(), value);
    Value::Object(patch)
}

/// A dropdown: an outline trigger showing the current choice and, while open, its menu.
fn select_control(
    ui: DialogUi<'_>,
    id: (&'static str, usize),
    menu: &str,
    width: f32,
    label: &str,
    shown: AnyElement,
    options: Vec<SelectOption>,
) -> Div {
    let theme = ui.theme;
    let open = ui.menu_open(menu);
    let toggle = ui.surface.clone();
    let toggle_menu = menu.to_string();
    let trigger = Button::new(id, theme)
        .variant(ButtonVariant::Outline)
        .trigger()
        .full_width()
        .open(open)
        .aria_label(label.to_string())
        .leading(shown)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_drawing_settings_menu(&toggle_menu, surface_cx);
            });
        });
    let panel = open.then(|| {
        MenuPanel::new(
            ("drawing_setting_menu", id.1),
            MenuPlacement::Anchored {
                side: MenuSide::Below,
                align: MenuAlign::Stretch,
            },
            theme,
        )
        .animate_from(PopupAnimationOrigin::TOP_LEFT)
        .children(options.into_iter().enumerate().map(|(index, option)| {
            let choose = ui.surface.clone();
            let patch = option.patch;
            MenuRow::compact(("drawing_setting_option", index), option.label, theme)
                .when_some(option.glyph, MenuRow::leading)
                .checked(option.checked)
                .on_click(move |_, _, cx| {
                    choose.update(cx, |surface, surface_cx| {
                        surface.choose_drawing_settings_option(&patch, surface_cx);
                    });
                })
        }))
    });
    let dismiss = ui.surface.clone();
    sized(
        width,
        MenuAnchor::new(("drawing_setting_select", id.1), trigger)
            .full_width()
            .menu(panel)
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, WorkspaceSurface::close_drawing_settings_menu);
            }),
    )
}

fn select_text(text: impl Into<SharedString>, theme: &AerisTheme) -> AnyElement {
    div()
        .min_w_0()
        .truncate()
        .text_sm()
        .text_color(gpui_color(theme.colors.text_default))
        .child(text.into())
        .into_any_element()
}

/// An enum property as a dropdown; line styles show their stroke.
fn enum_select(ui: DialogUi<'_>, property: &ChartDrawingProperty, width: f32) -> Div {
    let current = ui.settings.text(&property.name).to_string();
    let strokes = property.name == "style";
    let shown = if strokes {
        choice_glyph(&current, ui.theme)
    } else {
        select_text(humanize(&current), ui.theme)
    };
    let options = property
        .enum_values
        .iter()
        .map(|choice| SelectOption {
            label: humanize(choice),
            glyph: strokes.then(|| choice_glyph(choice, ui.theme)),
            checked: current == *choice,
            patch: single_option(&property.name, Value::String(choice.clone())),
        })
        .collect();
    select_control(
        ui,
        ("drawing_setting_enum", ui.slot(property)),
        &property.name,
        width,
        &property.label,
        shown,
        options,
    )
}

/// A width or font-size property as a dropdown of the sizes inside its bounds.
fn number_select(
    ui: DialogUi<'_>,
    property: &ChartDrawingProperty,
    menu: NumberMenu,
    width: f32,
) -> Div {
    let theme = ui.theme;
    let current = ui.settings.number(&property.name);
    let (shown, options): (String, Vec<SelectOption>) = match menu {
        NumberMenu::Width => (
            current.map_or_else(|| property.label.clone(), width_label),
            width_choices(property)
                .map(|size| SelectOption {
                    label: width_label(size),
                    glyph: Some(width_glyph(size, theme).into_any_element()),
                    checked: current == Some(size),
                    patch: single_option(&property.name, json!(size)),
                })
                .collect(),
        ),
        NumberMenu::FontSize => (
            current.map_or_else(|| "Auto".to_string(), number_text),
            std::iter::once(SelectOption {
                label: "Auto".to_string(),
                glyph: None,
                checked: current.is_none(),
                patch: single_option(&property.name, Value::Null),
            })
            .chain(font_size_choices(property).map(|size| SelectOption {
                label: number_text(size),
                glyph: None,
                checked: current == Some(size),
                patch: single_option(&property.name, json!(size)),
            }))
            .collect(),
        ),
    };
    select_control(
        ui,
        ("drawing_setting_number", ui.slot(property)),
        &property.name,
        width,
        &property.label,
        select_text(shown, theme),
        options,
    )
}

/// A color's swatch button and, while open, its picker.
fn color_control(
    ui: DialogUi<'_>,
    id: (&'static str, usize),
    label: SharedString,
    target: DrawingColorTarget,
    value: Option<String>,
) -> AnyElement {
    let theme = ui.theme;
    let open_picker = ui
        .dialog
        .color_picker
        .as_ref()
        .filter(|picker| picker.target == target);
    let shown = value.clone().unwrap_or_else(|| "Auto".to_string());
    let toggle = ui.surface.clone();
    let toggle_target = target.clone();
    let mut control = div().relative().flex_none().flex().child(
        Button::new(id, theme)
            .variant(ButtonVariant::Outline)
            .open(open_picker.is_some())
            .aria_label(label)
            .leading(color_swatch_dot(&shown, theme))
            .on_press(move |_, window, cx| {
                toggle.update(cx, |surface, surface_cx| {
                    surface.toggle_drawing_settings_color_picker(
                        toggle_target.clone(),
                        value.clone(),
                        window,
                        surface_cx,
                    );
                });
            }),
    );
    if let Some(picker) = open_picker {
        let select = ui.surface.clone();
        let dismiss = ui.surface.clone();
        control = control.child(gpui::deferred(
            ColorPicker::new(("drawing_color_popover", id.1), shown, &picker.input, theme)
                .error(picker.error.as_deref())
                .on_select(move |value, _, cx| {
                    select.update(cx, |surface, surface_cx| {
                        surface.set_drawing_settings_color(&target, &value, surface_cx);
                    });
                })
                .on_dismiss(move |_, cx| {
                    dismiss.update(cx, WorkspaceSurface::close_drawing_settings_color_picker);
                }),
        ));
    }
    control.into_any_element()
}

fn property_color(ui: DialogUi<'_>, property: &ChartDrawingProperty) -> AnyElement {
    color_control(
        ui,
        ("drawing_setting_color", ui.slot(property)),
        property.label.clone().into(),
        DrawingColorTarget::Property(property.name.clone()),
        ui.settings.color(&property.name),
    )
}

fn checkbox(ui: DialogUi<'_>, property: &ChartDrawingProperty) -> CheckboxRow {
    let update = ui.surface.clone();
    let name = property.name.clone();
    CheckboxRow::new(
        ("drawing_setting_check", ui.slot(property)),
        property.label.clone(),
        ui.settings.flag(&property.name),
        ui.theme,
    )
    .on_change(move |checked, _, _, cx| {
        update.update(cx, |surface, surface_cx| {
            surface.set_drawing_settings_value(&name, Value::Bool(checked), surface_cx);
        });
    })
}

/// A Bold or Italic switch drawn as its letter.
fn font_toggle(ui: DialogUi<'_>, property: &ChartDrawingProperty) -> Button {
    let theme = ui.theme;
    let on = ui.settings.flag(&property.name);
    let italic = property.name == "text_italic";
    let glyph = div()
        .w(px(14.0))
        .flex()
        .justify_center()
        .text_sm()
        .font_weight(platform_font_weight(if italic {
            TypographyRole::Normal
        } else {
            TypographyRole::Strong
        }))
        .when(italic, Styled::italic)
        .child(if italic { "I" } else { "B" });
    let update = ui.surface.clone();
    let name = property.name.clone();
    Button::new(("drawing_setting_font_toggle", ui.slot(property)), theme)
        .variant(ButtonVariant::Outline)
        .selected(on)
        .aria_label(property.label.clone())
        .tooltip(TooltipSpec::new(property.label.clone(), theme).show_delay(TOOLTIP_OPEN_DELAY))
        .leading(glyph)
        .on_press(move |_, _, cx| {
            update.update(cx, |surface, surface_cx| {
                surface.set_drawing_settings_value(&name, Value::Bool(!on), surface_cx);
            });
        })
}

fn extend_select(ui: DialogUi<'_>) -> Div {
    let left = ui.settings.flag("extend_left");
    let right = ui.settings.flag("extend_right");
    let current = EXTEND_CHOICES
        .iter()
        .find(|(_, l, r)| (*l, *r) == (left, right))
        .map_or("Don't extend", |(label, _, _)| label);
    let options = EXTEND_CHOICES
        .iter()
        .map(|&(label, l, r)| SelectOption {
            label: label.to_string(),
            glyph: None,
            checked: (l, r) == (left, right),
            patch: json!({ "extend_left": l, "extend_right": r }),
        })
        .collect();
    select_control(
        ui,
        ("drawing_setting_extend", 0),
        EXTEND_MENU,
        SELECT_WIDTH,
        "Extend",
        select_text(current, ui.theme),
        options,
    )
}

/// The control of one property that has a row of its own.
fn property_row(ui: DialogUi<'_>, property: &ChartDrawingProperty) -> Option<AnyElement> {
    let theme = ui.theme;
    let label = property.label.clone();
    Some(match property.property_type {
        Type::Boolean => checkbox(ui, property).into_any_element(),
        Type::Color => field_row(label, property_color(ui, property), theme).into_any_element(),
        Type::Enum => {
            field_row(label, enum_select(ui, property, SELECT_WIDTH), theme).into_any_element()
        }
        Type::Number | Type::Integer => {
            let control = match number_menu(property) {
                Some(menu) => number_select(ui, property, menu, COMPACT_SELECT_WIDTH),
                None => input_control(
                    ui.dialog.inputs.get(&property.name)?,
                    NUMBER_FIELD_WIDTH,
                    theme,
                ),
            };
            field_row(label, control, theme).into_any_element()
        }
        Type::String => {
            let input = ui.dialog.inputs.get(&property.name)?;
            field_row(
                label,
                div()
                    .flex_1()
                    .h(px(FIELD_HEIGHT))
                    .flex()
                    .child(Input::new(input).platform(theme).flex_1()),
                theme,
            )
            .into_any_element()
        }
        Type::Levels => levels_editor(ui, property),
        Type::Labels => labels_editor(ui, property),
        Type::IntervalSet => intervals_editor(ui),
    })
}

fn row_element(ui: DialogUi<'_>, row: &Row<'_>) -> Option<AnyElement> {
    let theme = ui.theme;
    Some(match row {
        Row::Line(parts) => field_row(
            "Line",
            controls().children(parts.iter().map(|part| stroke_control(ui, part))),
            theme,
        )
        .into_any_element(),
        Row::ToggledLine(flag, parts) => controls()
            .min_h(px(FIELD_HEIGHT + 4.0))
            .child(
                div()
                    .w(px(LABEL_WIDTH))
                    .flex_none()
                    .child(checkbox(ui, flag)),
            )
            .children(parts.iter().map(|part| stroke_control(ui, part)))
            .into_any_element(),
        Row::Extend => field_row("Extend", extend_select(ui), theme).into_any_element(),
        Row::Font(parts) => controls()
            .min_h(px(FIELD_HEIGHT + 4.0))
            .children(parts.iter().map(|part| {
                match part.property_type {
                    Type::Color => property_color(ui, part),
                    Type::Boolean => font_toggle(ui, part).into_any_element(),
                    _ => number_select(ui, part, NumberMenu::FontSize, COMPACT_SELECT_WIDTH)
                        .into_any_element(),
                }
            }))
            .into_any_element(),
        Row::TextBody => TextArea::new(ui.dialog.text_body.as_ref()?, px(TEXT_BODY_HEIGHT), theme)
            .into_any_element(),
        Row::Pair(label, first, second) => field_row(
            *label,
            controls()
                .child(enum_select(ui, first, SELECT_WIDTH))
                .child(enum_select(ui, second, SELECT_WIDTH)),
            theme,
        )
        .into_any_element(),
        Row::ToggledColor(flag, color) => controls()
            .min_h(px(FIELD_HEIGHT + 4.0))
            .child(
                div()
                    .w(px(LABEL_WIDTH))
                    .flex_none()
                    .child(checkbox(ui, flag)),
            )
            .child(property_color(ui, color))
            .into_any_element(),
        Row::Property(property) => property_row(ui, property)?,
    })
}

/// One stroke part: its color swatch, width menu or style menu.
fn stroke_control(ui: DialogUi<'_>, part: &ChartDrawingProperty) -> AnyElement {
    match part.property_type {
        Type::Color => property_color(ui, part),
        Type::Enum => enum_select(ui, part, COMPACT_SELECT_WIDTH).into_any_element(),
        _ => number_select(ui, part, NumberMenu::Width, COMPACT_SELECT_WIDTH).into_any_element(),
    }
}

/// Fib-style levels: a two-column grid of a checkbox, the value and the color per level.
fn levels_editor(ui: DialogUi<'_>, property: &ChartDrawingProperty) -> AnyElement {
    let theme = ui.theme;
    let slot = ui.slot(property);
    let name = property.name.clone();
    let levels = ui.settings.levels(&name);
    let inputs = ui.dialog.levels.get(&name);
    let cells = levels.iter().enumerate().map(|(index, level)| {
        let row_id = slot
            .saturating_mul(MAX_DRAWING_LEVELS + 1)
            .saturating_add(index);
        let toggle = ui.surface.clone();
        let remove = ui.surface.clone();
        let toggle_name = name.clone();
        let remove_name = name.clone();
        div()
            .w_1_2()
            .flex()
            .items_center()
            .gap_1p5()
            .py_0p5()
            .child(
                CheckboxRow::new(("drawing_level_visible", row_id), "", level.visible, theme)
                    .on_change(move |checked, _, _, cx| {
                        toggle.update(cx, |surface, surface_cx| {
                            surface.set_drawing_level_visible(
                                &toggle_name,
                                index,
                                checked,
                                surface_cx,
                            );
                        });
                    }),
            )
            .children(
                inputs
                    .and_then(|inputs| inputs.get(index))
                    .map(|input| input_control(input, LEVEL_FIELD_WIDTH, theme)),
            )
            .child(color_control(
                ui,
                ("drawing_level_color", row_id),
                format!("Level {} color", index + 1).into(),
                DrawingColorTarget::Level {
                    property: name.clone(),
                    index,
                },
                aeris_chart_integration::drawing_color_hex(&level.color),
            ))
            .child(
                Button::new(("drawing_level_remove", row_id), theme)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Sm)
                    .icon(Icon::new(HugeIcon::Trash.path()))
                    .aria_label("Remove level")
                    .danger_on_hover()
                    .on_press(move |_, window, cx| {
                        remove.update(cx, |surface, surface_cx| {
                            surface.remove_drawing_level(&remove_name, index, window, surface_cx);
                        });
                    }),
            )
    });
    let add = ui.surface.clone();
    let add_name = name.clone();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(section_title(property.label.clone(), theme))
        .child(div().flex().flex_wrap().children(cells))
        .child(
            div().flex().child(
                Button::new(("drawing_level_add", slot), theme)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Sm)
                    .icon(Icon::new(HugeIcon::Add.path()))
                    .label("Add level")
                    .disabled(levels.len() >= MAX_DRAWING_LEVELS)
                    .on_press(move |_, window, cx| {
                        add.update(cx, |surface, surface_cx| {
                            surface.add_drawing_level(&add_name, window, surface_cx);
                        });
                    }),
            ),
        )
        .into_any_element()
}

/// The stats a kind can show on its card, as a two-column checklist.
fn labels_editor(ui: DialogUi<'_>, property: &ChartDrawingProperty) -> AnyElement {
    let slot = ui.slot(property);
    let labels = ui
        .settings
        .value("labels")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let cells = property
        .enum_values
        .iter()
        .enumerate()
        .map(|(index, metric)| {
            let visible = labels.iter().any(|label| {
                label.get("metric").and_then(Value::as_str) == Some(metric.as_str())
                    && label.get("visible").and_then(Value::as_bool) == Some(true)
            });
            let update = ui.surface.clone();
            let metric = metric.clone();
            div().w_1_2().py_0p5().child(
                CheckboxRow::new(
                    (
                        "drawing_label_metric",
                        slot.saturating_mul(32).saturating_add(index),
                    ),
                    humanize(&metric),
                    visible,
                    ui.theme,
                )
                .on_change(move |checked, _, _, cx| {
                    update.update(cx, |surface, surface_cx| {
                        surface.set_drawing_label_visible(&metric, checked, surface_cx);
                    });
                }),
            )
        });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(section_title("Info", ui.theme))
        .child(div().flex().flex_wrap().children(cells))
        .into_any_element()
}

fn intervals_editor(ui: DialogUi<'_>) -> AnyElement {
    let theme = ui.theme;
    let visibility = ui.settings.interval_visibility();
    let rows = INTERVAL_UNITS
        .iter()
        .enumerate()
        .zip(&ui.dialog.intervals)
        .map(|((slot, &(unit, label, _, _)), inputs)| {
            let toggle = ui.surface.clone();
            controls()
                .min_h(px(FIELD_HEIGHT + 4.0))
                .child(
                    div().w(px(LABEL_WIDTH)).flex_none().child(
                        CheckboxRow::new(
                            ("drawing_interval_unit", slot),
                            label,
                            unit_checked(&visibility, unit),
                            theme,
                        )
                        .on_change(move |_, _, _, cx| {
                            toggle.update(cx, |surface, surface_cx| {
                                surface.apply_drawing_interval_visibility(Some(slot), surface_cx);
                            });
                        }),
                    ),
                )
                .child(input_control(&inputs.min, INTERVAL_FIELD_WIDTH, theme))
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(theme.colors.text_muted))
                        .child("to"),
                )
                .child(input_control(&inputs.max, INTERVAL_FIELD_WIDTH, theme))
        });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(rows)
        .into_any_element()
}

fn anchors_editor(ui: DialogUi<'_>) -> impl IntoElement + use<> {
    let theme = ui.theme;
    let rows = ui.dialog.anchors.iter().map(|anchor| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(field_row(
                anchor.label.clone(),
                controls()
                    .when(anchor.show_price, |row| {
                        row.child(input_control(&anchor.price, NUMBER_FIELD_WIDTH, theme))
                    })
                    .children(
                        anchor
                            .time
                            .as_ref()
                            .map(|time| input_control(time, 148.0, theme)),
                    ),
                theme,
            ))
            .children(anchor.error.clone().map(|error| {
                div()
                    .pl(px(LABEL_WIDTH + 12.0))
                    .text_xs()
                    .text_color(gpui_color(theme.colors.danger))
                    .child(error)
            }))
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(rows)
        .when(!ui.dialog.anchors.is_empty(), |editor| {
            editor.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(gpui_color(theme.colors.text_muted))
                    .child("Times are YYYY-MM-DD HH:MM in the chart's time zone. Press Enter to move a point."),
            )
        })
}

fn tab_strip(ui: DialogUi<'_>) -> impl IntoElement + use<> {
    let tabs = DIALOG_GROUPS
        .iter()
        .enumerate()
        .filter(|(_, (group, _))| group_has_content(ui.settings, *group))
        .map(|(index, &(group, label))| {
            let select = ui.surface.clone();
            Tab::new(("drawing_settings_tab", index), ui.theme)
                .selected(ui.dialog.tab == group)
                .child(label)
                .on_click(move |_, _, cx| {
                    select.update(cx, |surface, surface_cx| {
                        surface.select_drawing_settings_tab(group, surface_cx);
                    });
                })
        });
    div()
        .flex()
        .pb_1()
        .child(TabList::new("drawing_settings_tabs", "Drawing settings", ui.theme).children(tabs))
}

pub(super) fn drawing_settings_dialog_layer(
    surface: &Entity<WorkspaceSurface>,
    dialog: &DrawingSettingsDialogState,
    theme: &AerisTheme,
    cx: &App,
) -> Option<AnyElement> {
    let settings = dialog.chart.read(cx).drawing_settings(dialog.drawing)?;
    let ui = DialogUi {
        surface,
        dialog,
        settings: &settings,
        theme,
    };
    let rows: Vec<AnyElement> = dialog_rows(&settings.schema.properties, dialog.tab)
        .iter()
        .filter_map(|row| row_element(ui, row))
        .collect();
    let dismiss = surface.clone();
    let cancel = surface.clone();
    let confirm = surface.clone();
    let mover = surface.clone();
    Some(
        Dialog::new(
            ("drawing_settings_dialog", surface.entity_id().as_u64()),
            DialogSize::Sm,
            theme,
            move |window, cx| {
                dismiss.update(cx, |surface, surface_cx| {
                    surface.cancel_drawing_settings_dialog(window, surface_cx);
                });
            },
        )
        .title(dialog.title)
        .movable(dialog.origin, move |step, cx| {
            mover.update(cx, |surface, surface_cx| {
                surface.move_drawing_settings_dialog(step, surface_cx);
            });
        })
        .track_focus(&dialog.focus)
        .max_height(px(DIALOG_MAX_HEIGHT))
        .child(tab_strip(ui))
        .when(dialog.tab == Group::Coordinates, |body| {
            body.child(anchors_editor(ui))
        })
        .children(rows)
        .action(
            Button::new("drawing_settings_cancel", theme)
                .variant(ButtonVariant::Outline)
                .label("Cancel")
                .on_click(move |_, window, cx| {
                    cancel.update(cx, |surface, surface_cx| {
                        surface.cancel_drawing_settings_dialog(window, surface_cx);
                    });
                }),
        )
        .action(
            Button::new("drawing_settings_ok", theme)
                .variant(ButtonVariant::Primary)
                .label("Ok")
                .on_click(move |_, window, cx| {
                    confirm.update(cx, |surface, surface_cx| {
                        surface.confirm_drawing_settings_dialog(window, surface_cx);
                    });
                }),
        )
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(property_type: Type, min: Option<f64>, max: Option<f64>) -> ChartDrawingProperty {
        named("width", Group::Style, property_type, min, max)
    }

    fn named(
        name: &str,
        group: Group,
        property_type: Type,
        min: Option<f64>,
        max: Option<f64>,
    ) -> ChartDrawingProperty {
        ChartDrawingProperty {
            name: name.to_string(),
            label: humanize(name),
            group,
            toolbar: false,
            property_type,
            default: Value::Null,
            min,
            max,
            enum_values: Vec::new(),
        }
    }

    #[test]
    fn typed_numbers_apply_only_inside_the_schema_bounds() {
        let width = property(Type::Number, Some(1.0), Some(4.0));
        assert_eq!(bounded_number(" 2.5 ", &width), Some(2.5));
        assert_eq!(bounded_number("0.5", &width), None);
        assert_eq!(bounded_number("5", &width), None);
        assert_eq!(bounded_number("NaN", &width), None);
        assert_eq!(bounded_number("", &width), None);
        let order = property(Type::Integer, None, None);
        assert_eq!(bounded_number("3", &order), Some(3.0));
        assert_eq!(bounded_number("3.5", &order), None);
    }

    #[test]
    fn engine_identifiers_read_as_sentence_case() {
        assert_eq!(humanize("line_open"), "Line open");
        assert_eq!(humanize("solid"), "Solid");
        assert_eq!(humanize(""), "");
    }

    #[test]
    fn widths_and_font_sizes_are_menus_and_other_numbers_are_typed() {
        let font = named(
            "text_size",
            Group::Text,
            Type::Number,
            Some(6.0),
            Some(96.0),
        );
        assert_eq!(number_menu(&font), Some(NumberMenu::FontSize));
        assert_eq!(
            font_size_choices(&font).collect::<Vec<_>>(),
            FONT_SIZES.to_vec()
        );
        let small = named(
            "text_size",
            Group::Text,
            Type::Number,
            Some(11.0),
            Some(20.0),
        );
        assert_eq!(
            font_size_choices(&small).collect::<Vec<_>>(),
            vec![11.0, 12.0, 14.0, 16.0, 20.0]
        );
        let border = named("box_border_width", Group::Text, Type::Number, None, None);
        assert_eq!(number_menu(&border), Some(NumberMenu::Width));
        let risk = named(
            "position_risk_percent",
            Group::Inputs,
            Type::Number,
            None,
            None,
        );
        assert_eq!(number_menu(&risk), None);
        let style = named("style", Group::Style, Type::Enum, None, None);
        assert_eq!(number_menu(&style), None);
    }

    #[test]
    fn drawing_text_stays_on_one_line() {
        assert_eq!(single_line("Break\nout\r\tnow"), "Break out  now");
        assert_eq!(single_line("plain"), "plain");
    }

    #[test]
    fn related_properties_share_one_row_in_schema_order() {
        let properties = vec![
            named("color", Group::Style, Type::Color, None, None),
            named("width", Group::Style, Type::Number, Some(1.0), Some(4.0)),
            named("style", Group::Style, Type::Enum, None, None),
            named("stroke_start", Group::Style, Type::Enum, None, None),
            named("stroke_end", Group::Style, Type::Enum, None, None),
            named("extend_left", Group::Style, Type::Boolean, None, None),
            named("extend_right", Group::Style, Type::Boolean, None, None),
            named(
                "middle_point_visible",
                Group::Style,
                Type::Boolean,
                None,
                None,
            ),
            named("border_visible", Group::Style, Type::Boolean, None, None),
            named(
                "middle_line_visible",
                Group::Style,
                Type::Boolean,
                None,
                None,
            ),
            named("middle_line_color", Group::Style, Type::Color, None, None),
            named(
                "middle_line_width",
                Group::Style,
                Type::Number,
                Some(1.0),
                Some(4.0),
            ),
            named("middle_line_style", Group::Style, Type::Enum, None, None),
            named("fill_enabled", Group::Style, Type::Boolean, None, None),
            named("fill_color", Group::Style, Type::Color, None, None),
            named("text_color", Group::Text, Type::Color, None, None),
            named("text_size", Group::Text, Type::Number, None, None),
            named("text_bold", Group::Text, Type::Boolean, None, None),
            named("text_italic", Group::Text, Type::Boolean, None, None),
            named("text", Group::Text, Type::String, None, None),
            named("text_v_align", Group::Text, Type::Enum, None, None),
            named("text_h_align", Group::Text, Type::Enum, None, None),
            named("name", Group::General, Type::String, None, None),
        ];
        let names = |rows: Vec<Row<'_>>| -> Vec<String> {
            rows.iter()
                .map(|row| match row {
                    Row::Line(parts) => format!("line:{}", parts.len()),
                    Row::Extend => "extend".to_string(),
                    Row::Font(parts) => format!("font:{}", parts.len()),
                    Row::TextBody => "text".to_string(),
                    Row::Pair(label, _, _) => format!("pair:{label}"),
                    Row::ToggledColor(flag, _) => format!("toggle:{}", flag.name),
                    Row::ToggledLine(flag, parts) => {
                        format!("toggled_line:{}:{}", flag.name, parts.len())
                    }
                    Row::Property(property) => property.name.clone(),
                })
                .collect()
        };
        assert_eq!(
            names(dialog_rows(&properties, Group::Style)),
            [
                "line:3",
                "pair:Line ends",
                "extend",
                "middle_point_visible",
                "border_visible",
                "toggled_line:middle_line_visible:3",
                "toggle:fill_enabled"
            ]
        );
        assert_eq!(
            names(dialog_rows(&properties, Group::Text)),
            ["font:4", "text", "pair:Text alignment"]
        );
        assert_eq!(dialog_rows(&properties, Group::General).len(), 1);
    }

    #[test]
    fn a_lone_member_of_a_group_keeps_its_own_row() {
        let properties = vec![
            named("color", Group::Style, Type::Color, None, None),
            named("extend_right", Group::Style, Type::Boolean, None, None),
            named("level_label_align", Group::Style, Type::Enum, None, None),
            named("fill_enabled", Group::Style, Type::Boolean, None, None),
        ];
        let rows = dialog_rows(&properties, Group::Style);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|row| matches!(row, Row::Property(_))));
    }

    #[test]
    fn a_unit_is_checked_while_visibility_is_off_or_it_has_a_range() {
        let mut visibility = ChartDrawingIntervalVisibility::default();
        assert!(unit_checked(&visibility, ChartDrawingIntervalUnit::Minutes));
        visibility = serde_json::from_value(json!({
            "enabled": true,
            "intervals": [],
            "ranges": [{ "unit": "hours", "min": 1.0, "max": 4.0 }],
        }))
        .expect("visibility");
        assert!(unit_checked(&visibility, ChartDrawingIntervalUnit::Hours));
        assert!(!unit_checked(
            &visibility,
            ChartDrawingIntervalUnit::Minutes
        ));
        assert_eq!(
            unit_range(&visibility, ChartDrawingIntervalUnit::Hours, (1.0, 24.0)),
            (1.0, 4.0)
        );
        assert_eq!(
            unit_range(&visibility, ChartDrawingIntervalUnit::Days, (1.0, 366.0)),
            (1.0, 366.0)
        );
    }
}
