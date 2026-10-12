//! The drawing settings dialog: one form for every Aeris Charts drawing kind, built from the
//! kind's property schema. Edits preview live inside an Aeris Charts settings session; Ok keeps
//! them as one undo step and Cancel restores the drawing as it was.

use super::*;
use aeris_chart_integration::{
    ChartDrawingIntervalUnit, ChartDrawingIntervalVisibility, ChartDrawingLevel,
    ChartDrawingProperty, ChartDrawingPropertyGroup as Group, ChartDrawingPropertyType as Type,
    ChartDrawingSettings, MAX_DRAWING_LEVELS,
};
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

/// An open drawing settings dialog. The drawing itself is the draft: every control writes to
/// Aeris Charts inside the session `chart` began, and the form reads the drawing back.
pub(super) struct DrawingSettingsDialogState {
    chart: Entity<AerisChartView>,
    drawing: ChartDrawingId,
    title: &'static str,
    tab: Group,
    /// Text fields of number and string properties, by property name.
    inputs: HashMap<String, Entity<InputState>>,
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

fn property_shown(property: &ChartDrawingProperty) -> bool {
    property.group != Group::General
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
fn humanize(identifier: &str) -> String {
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
        let _ = surface.update(cx, |surface, surface_cx| {
            surface.drawing_settings_input_event(&target, event, surface_cx);
        });
    })
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
        let mut levels = HashMap::new();
        let mut level_subscriptions = HashMap::new();
        for property in settings
            .schema
            .properties
            .iter()
            .filter(|p| property_shown(p))
        {
            let name = property.name.clone();
            let text = match property.property_type {
                Type::Number | Type::Integer => {
                    settings.number(&name).map(number_text).unwrap_or_default()
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
            inputs,
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
    const fn drawing_settings_dialog(&self) -> Option<&DrawingSettingsDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::Drawing(dialog)) => Some(dialog),
            _ => None,
        }
    }

    const fn drawing_settings_dialog_mut(&mut self) -> Option<&mut DrawingSettingsDialogState> {
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
            self.indicator_message = Some("This drawing cannot be edited right now".to_string());
            cx.notify();
            return;
        }
        let dialog = DrawingSettingsDialogState::new(chart, &settings, window, cx);
        self.open_indicator_settings_dialog(IndicatorSettingsDialog::Drawing(dialog), cx);
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
    pub(super) fn confirm_drawing_settings_dialog(&mut self, cx: &mut Context<Self>) {
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
    }

    pub(super) fn select_drawing_settings_tab(&mut self, tab: Group, cx: &mut Context<Self>) {
        if let Some(dialog) = self.drawing_settings_dialog_mut()
            && dialog.tab != tab
        {
            dialog.tab = tab;
            dialog.color_picker = None;
            cx.notify();
        }
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

    pub(super) fn set_drawing_settings_value(
        &mut self,
        name: &str,
        value: Value,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn set_drawing_level_visible(
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
    pub(super) fn add_drawing_level(
        &mut self,
        property: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                style: "solid".to_string(),
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

    pub(super) fn remove_drawing_level(
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
    pub(super) fn set_drawing_label_visible(
        &mut self,
        metric: &str,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn apply_drawing_interval_visibility(
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

fn field_row(label: impl Into<SharedString>, control: impl IntoElement, theme: &AerisTheme) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .min_h(px(32.0))
        .child(
            div()
                .flex_none()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(label.into()),
        )
        .child(control)
}

fn section_title(title: impl Into<SharedString>, theme: &AerisTheme) -> Div {
    div()
        .pt_1()
        .text_xs()
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .text_color(gpui_color(theme.colors.text_muted))
        .child(title.into())
}

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
    let mut control = div().relative().flex().child(
        color_swatch_button(id, label, &shown, open_picker.is_some(), theme).on_press(
            move |_, window, cx| {
                toggle.update(cx, |surface, surface_cx| {
                    surface.toggle_drawing_settings_color_picker(
                        toggle_target.clone(),
                        value.clone(),
                        window,
                        surface_cx,
                    );
                });
            },
        ),
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

fn input_control(input: &Entity<InputState>, width: f32, theme: &AerisTheme) -> Div {
    div()
        .w(px(width))
        .h(px(32.0))
        .flex()
        .child(Input::new(input).platform(theme).flex_1())
}

fn enum_control(ui: DialogUi<'_>, slot: usize, property: &ChartDrawingProperty) -> AnyElement {
    let current = ui.settings.text(&property.name).to_string();
    let tabs = property
        .enum_values
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let update = ui.surface.clone();
            let name = property.name.clone();
            let value = choice.clone();
            Tab::new(
                (
                    "drawing_setting_choice",
                    slot.saturating_mul(32).saturating_add(index),
                ),
                ui.theme,
            )
            .selected(current == *choice)
            .child(humanize(choice))
            .on_click(move |_, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_drawing_settings_value(
                        &name,
                        Value::String(value.clone()),
                        surface_cx,
                    );
                });
            })
        });
    div()
        .flex()
        .child(
            TabList::new(
                ("drawing_setting_choices", slot),
                property.label.clone(),
                ui.theme,
            )
            .wrap()
            .children(tabs),
        )
        .into_any_element()
}

fn property_control(
    ui: DialogUi<'_>,
    slot: usize,
    property: &ChartDrawingProperty,
) -> Option<AnyElement> {
    let theme = ui.theme;
    let name = property.name.clone();
    let label = property.label.clone();
    Some(match property.property_type {
        Type::Boolean => {
            let update = ui.surface.clone();
            Switch::new(
                ("drawing_setting_switch", slot),
                label,
                ui.settings.flag(&name),
                theme,
            )
            .on_change(move |checked, _, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_drawing_settings_value(&name, Value::Bool(checked), surface_cx);
                });
            })
            .into_any_element()
        }
        Type::Number | Type::Integer => {
            let input = ui.dialog.inputs.get(&name)?;
            field_row(label, input_control(input, 120.0, theme), theme).into_any_element()
        }
        Type::String => {
            let input = ui.dialog.inputs.get(&name)?;
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(theme.colors.text_secondary))
                        .child(label),
                )
                .child(
                    div()
                        .h(px(32.0))
                        .flex()
                        .child(Input::new(input).platform(theme).flex_1()),
                )
                .into_any_element()
        }
        Type::Color => field_row(
            label.clone(),
            color_control(
                ui,
                ("drawing_setting_color", slot),
                label.into(),
                DrawingColorTarget::Property(name.clone()),
                ui.settings.color(&name),
            ),
            theme,
        )
        .into_any_element(),
        Type::Enum => field_row(label, enum_control(ui, slot, property), theme).into_any_element(),
        Type::Levels => levels_editor(ui, slot, property),
        Type::Labels => labels_editor(ui, slot, property),
        Type::IntervalSet => intervals_editor(ui),
    })
}

fn levels_editor(ui: DialogUi<'_>, slot: usize, property: &ChartDrawingProperty) -> AnyElement {
    let theme = ui.theme;
    let name = property.name.clone();
    let levels = ui.settings.levels(&name);
    let inputs = ui.dialog.levels.get(&name);
    let rows = levels.iter().enumerate().map(|(index, level)| {
        let row_id = slot
            .saturating_mul(MAX_DRAWING_LEVELS + 1)
            .saturating_add(index);
        let toggle = ui.surface.clone();
        let remove = ui.surface.clone();
        let toggle_name = name.clone();
        let remove_name = name.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
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
                    .map(|input| input_control(input, 96.0, theme)),
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
        .gap_2()
        .child(section_title(property.label.clone(), theme))
        .children(rows)
        .child(
            div().flex().child(
                Button::new(("drawing_level_add", slot), theme)
                    .variant(ButtonVariant::Outline)
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

fn labels_editor(ui: DialogUi<'_>, slot: usize, property: &ChartDrawingProperty) -> AnyElement {
    let labels = ui
        .settings
        .value("labels")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let rows = property
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
            })
        });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(section_title(property.label.clone(), ui.theme))
        .children(rows)
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
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
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
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(input_control(&inputs.min, 72.0, theme))
                        .child(
                            div()
                                .text_sm()
                                .text_color(gpui_color(theme.colors.text_muted))
                                .child("to"),
                        )
                        .child(input_control(&inputs.max, 72.0, theme)),
                )
        });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child("Show the drawing only on the checked intervals and ranges."),
        )
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(gpui_color(theme.colors.text_secondary))
                            .child(anchor.label.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(anchor.show_price, |row| {
                                row.child(input_control(&anchor.price, 120.0, theme))
                            })
                            .children(
                                anchor
                                    .time
                                    .as_ref()
                                    .map(|time| input_control(time, 160.0, theme)),
                            ),
                    ),
            )
            .children(anchor.error.clone().map(|error| {
                div()
                    .text_xs()
                    .text_color(gpui_color(theme.colors.danger))
                    .child(error)
            }))
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(rows)
        .when(!ui.dialog.anchors.is_empty(), |editor| {
            editor.child(
                div()
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
    let controls = settings
        .schema
        .properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.group == dialog.tab)
        .filter_map(|(slot, property)| property_control(ui, slot, property));
    let dismiss = surface.clone();
    let cancel = surface.clone();
    let confirm = surface.clone();
    Some(
        Dialog::new(
            "drawing_settings_dialog",
            DialogSize::Md,
            theme,
            move |_, cx| {
                dismiss.update(cx, |surface, surface_cx| {
                    surface.close_drawing_settings_dialog(false, surface_cx);
                });
            },
        )
        .title(dialog.title)
        .subtitle("Drawing settings")
        .max_height(px(680.0))
        .child(tab_strip(ui))
        .when(dialog.tab == Group::Coordinates, |body| {
            body.child(anchors_editor(ui))
        })
        .children(controls)
        .action(
            Button::new("drawing_settings_cancel", theme)
                .variant(ButtonVariant::Outline)
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, |surface, surface_cx| {
                        surface.close_drawing_settings_dialog(false, surface_cx);
                    });
                }),
        )
        .action(
            Button::new("drawing_settings_ok", theme)
                .variant(ButtonVariant::Secondary)
                .label("Ok")
                .on_click(move |_, _, cx| {
                    confirm.update(cx, WorkspaceSurface::confirm_drawing_settings_dialog);
                }),
        )
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(property_type: Type, min: Option<f64>, max: Option<f64>) -> ChartDrawingProperty {
        ChartDrawingProperty {
            name: "width".to_string(),
            label: "Width".to_string(),
            group: Group::Style,
            toolbar: true,
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
