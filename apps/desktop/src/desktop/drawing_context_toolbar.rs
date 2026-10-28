//! The floating toolbar of the active chart's selected drawing. Its quick controls come from the
//! Aeris Charts property schema of the drawing's kind (the properties flagged `toolbar`), so every
//! kind gets the controls it paints without a per-kind toolbar. With several drawings selected
//! it shows the primary drawing's controls, and style, lock, hide, clone and remove act on the
//! whole selection (each drawing takes only the settings its kind has).

use super::*;
use aeris_chart_integration::{
    ChartDrawingProperty, ChartDrawingPropertyType as Type, ChartDrawingSettings,
    MAX_DRAWING_TEMPLATE_BYTES,
};
use chart_context_menus::{chart_css_color, color_swatch_dot};
use drawing_settings_dialog::humanize;
use drawing_toolbar::{
    FLOATING_TOOLBAR_BUTTON_SIZE, FLOATING_TOOLBAR_DEFAULT_INSET, FLOATING_TOOLBAR_GAP,
    clamp_floating_toolbar_origin, floating_toolbar_grip, floating_toolbar_panel,
    floating_toolbar_size,
};
use local_state::{MAXIMUM_DRAWING_TEMPLATE_NAME_BYTES, MAXIMUM_DRAWING_TEMPLATES};
use serde_json::Value;

/// The widths a toolbar width menu offers, within the property's own bounds.
const LINE_WIDTHS: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
const ORDER_BUTTON_WIDTH: f32 = 128.0;
const TOOLBAR_MENU_WIDTH: f32 = 200.0;
const TEMPLATE_MENU_WIDTH: f32 = 240.0;
/// The Text tool glyph above its color bar, sized so both fit one toolbar button.
const TEXT_GLYPH_SIZE: f32 = 15.0;

/// The drawing the toolbar is showing, so a new selection closes what the old one had open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToolbarTarget {
    chart: gpui::EntityId,
    drawing: ChartDrawingId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ToolbarMenu {
    /// The menu of one numeric or choice property.
    Property(String),
    Templates,
    More,
}

/// An open color picker. Its edits preview inside an Aeris Charts settings session, so dragging
/// across the color field becomes one undo step when the picker closes.
struct ToolbarColorPicker {
    surface: Entity<WorkspaceSurface>,
    chart: Entity<AerisChartView>,
    property: String,
    input: Entity<InputState>,
    error: Option<String>,
}

struct TemplateNameField {
    input: Entity<InputState>,
    _submit: gpui::Subscription,
}

/// Terminal state of the floating drawing toolbar. The drawing and its options stay in Aeris
/// Charts; this only tracks what the toolbar itself has open.
#[derive(Default)]
pub(super) struct DrawingContextToolbar {
    target: Option<ToolbarTarget>,
    grab: Option<gpui::Point<Pixels>>,
    menu: Option<ToolbarMenu>,
    color_picker: Option<ToolbarColorPicker>,
    template_name: Option<TemplateNameField>,
    template_error: Option<String>,
}

/// The selected drawing of the active pane's chart, as the toolbar edits it.
struct SelectedDrawing {
    surface: Entity<WorkspaceSurface>,
    chart: Entity<AerisChartView>,
    settings: ChartDrawingSettings,
}

/// One quick control, in schema order.
enum ToolbarControl<'a> {
    Color(&'a ChartDrawingProperty),
    Width(&'a ChartDrawingProperty),
    Choice(&'a ChartDrawingProperty),
}

fn toolbar_controls(settings: &ChartDrawingSettings) -> Vec<ToolbarControl<'_>> {
    settings
        .schema
        .properties
        .iter()
        .filter(|property| property.toolbar)
        .filter_map(|property| match property.property_type {
            Type::Color => Some(ToolbarControl::Color(property)),
            Type::Number | Type::Integer => Some(ToolbarControl::Width(property)),
            Type::Enum if !property.enum_values.is_empty() => {
                Some(ToolbarControl::Choice(property))
            }
            _ => None,
        })
        .collect()
}

const fn is_position(kind: ChartDrawingKind) -> bool {
    matches!(
        kind,
        ChartDrawingKind::LongPosition | ChartDrawingKind::ShortPosition
    )
}

/// Template, the quick controls, then settings, lock, delete and more.
fn toolbar_size(settings: &ChartDrawingSettings, border_width: f32) -> gpui::Size<Pixels> {
    let buttons = toolbar_controls(settings).len() + 5;
    let mut size = floating_toolbar_size(buttons, border_width);
    if is_position(settings.kind()) {
        size.width += px(FLOATING_TOOLBAR_GAP + ORDER_BUTTON_WIDTH);
    }
    size
}

/// Where the toolbar opens until it is first dragged: centered just below the chart header.
fn default_origin(
    chart_top: f32,
    viewport: gpui::Size<Pixels>,
    toolbar: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    point(
        (viewport.width - toolbar.width) / 2.0,
        px(chart_top + FLOATING_TOOLBAR_DEFAULT_INSET),
    )
}

pub(super) fn width_choices(property: &ChartDrawingProperty) -> impl Iterator<Item = f64> + '_ {
    LINE_WIDTHS.into_iter().filter(|width| {
        property.min.is_none_or(|min| *width >= min) && property.max.is_none_or(|max| *width <= max)
    })
}

pub(super) fn width_label(width: f64) -> String {
    format!("{width}px")
}

/// A template name the workspace sanitizer keeps.
fn valid_template_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAXIMUM_DRAWING_TEMPLATE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

fn single_option(name: &str, value: Value) -> Value {
    let mut patch = serde_json::Map::new();
    patch.insert(name.to_string(), value);
    Value::Object(patch)
}

/// Drag payload for moving the toolbar by its grip.
#[derive(Clone)]
struct DrawingContextMoveDrag;

impl Render for DrawingContextMoveDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

impl TerminalApp {
    /// The drawing the toolbar shows: the active chart's selection, unless a settings dialog
    /// is editing that pane.
    fn toolbar_drawing(&self, cx: &App) -> Option<SelectedDrawing> {
        let surface = self.active_surface();
        let workspace = surface.read(cx);
        if workspace.indicator_settings_dialog.is_some() {
            return None;
        }
        let chart = workspace.chart.clone()?;
        let settings = chart.read(cx).selected_drawing_settings()?;
        Some(SelectedDrawing {
            surface: surface.clone(),
            chart,
            settings,
        })
    }

    fn toolbar_target(&self, cx: &App) -> Option<ToolbarTarget> {
        let workspace = self.active_surface().read(cx);
        if workspace.indicator_settings_dialog.is_some() {
            return None;
        }
        let chart = workspace.chart.as_ref()?;
        Some(ToolbarTarget {
            chart: chart.entity_id(),
            drawing: chart.read(cx).selected_drawing_id()?,
        })
    }

    /// Closes whatever the toolbar had open once its drawing is no longer the one selected.
    pub(super) fn sync_drawing_context_toolbar(&mut self, cx: &mut Context<Self>) {
        let target = self.toolbar_target(cx);
        if self.chrome.context_toolbar().target == target {
            return;
        }
        self.end_drawing_toolbar_color_edit(cx);
        let toolbar = self.chrome.context_toolbar_mut();
        toolbar.target = target;
        toolbar.menu = None;
        toolbar.template_name = None;
        toolbar.template_error = None;
    }

    fn toggle_drawing_toolbar_menu(&mut self, menu: ToolbarMenu, cx: &mut Context<Self>) {
        self.end_drawing_toolbar_color_edit(cx);
        let toolbar = self.chrome.context_toolbar_mut();
        toolbar.menu = (toolbar.menu.as_ref() != Some(&menu)).then_some(menu);
        toolbar.template_name = None;
        toolbar.template_error = None;
        cx.notify();
    }

    fn close_drawing_toolbar_menu(&mut self, cx: &mut Context<Self>) {
        let toolbar = self.chrome.context_toolbar_mut();
        if toolbar.menu.take().is_some() {
            toolbar.template_name = None;
            toolbar.template_error = None;
            cx.notify();
        }
    }

    /// One option change for every selected drawing. Outside the color picker's session it is
    /// its own undo step.
    fn apply_drawing_toolbar_option(&mut self, name: &str, value: Value, cx: &mut Context<Self>) {
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        let patch = single_option(name, value);
        selected.chart.update(cx, |chart, chart_cx| {
            if chart.apply_selected_drawings_options(&patch) {
                chart_cx.notify();
            }
        });
        cx.notify();
    }

    fn choose_drawing_toolbar_option(&mut self, name: &str, value: Value, cx: &mut Context<Self>) {
        self.apply_drawing_toolbar_option(name, value, cx);
        self.close_drawing_toolbar_menu(cx);
    }

    fn toggle_drawing_toolbar_color(
        &mut self,
        property: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reopen = self
            .chrome
            .context_toolbar()
            .color_picker
            .as_ref()
            .is_none_or(|picker| picker.property != property);
        self.end_drawing_toolbar_color_edit(cx);
        self.close_drawing_toolbar_menu(cx);
        cx.notify();
        if !reopen {
            return;
        }
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        if !selected
            .chart
            .update(cx, |chart, _| chart.begin_selected_drawings_settings_edit())
        {
            return;
        }
        let current = selected
            .settings
            .color(property)
            .unwrap_or_default()
            .to_ascii_uppercase();
        let input = cx.new(|input_cx| InputState::new(window, input_cx));
        input.update(cx, |input, input_cx| {
            input.set_value(current, window, input_cx);
        });
        self.chrome.context_toolbar_mut().color_picker = Some(ToolbarColorPicker {
            surface: selected.surface,
            chart: selected.chart,
            property: property.to_string(),
            input,
            error: None,
        });
    }

    fn close_drawing_toolbar_color(&mut self, cx: &mut Context<Self>) {
        self.end_drawing_toolbar_color_edit(cx);
        cx.notify();
    }

    /// Ends the picker's session, keeping its edits as one undo step.
    fn end_drawing_toolbar_color_edit(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.chrome.context_toolbar_mut().color_picker.take() else {
            return;
        };
        // A settings dialog opened since kept the picker's edits and owns the session now.
        if picker.surface.read(cx).drawing_settings_dialog_open() {
            return;
        }
        picker.chart.update(cx, |chart, chart_cx| {
            if chart.commit_drawing_settings_edit() {
                chart_cx.notify();
            }
        });
    }

    fn set_drawing_toolbar_color(&mut self, color: &str, cx: &mut Context<Self>) {
        let Some(picker) = self.chrome.context_toolbar_mut().color_picker.as_mut() else {
            return;
        };
        let normalized = normalize_hex_color(color);
        picker.error = normalized
            .is_none()
            .then(|| "Enter a hex color such as #2962FF or #2962FF80.".to_string());
        let property = picker.property.clone();
        match normalized {
            Some(color) => self.apply_drawing_toolbar_option(&property, Value::String(color), cx),
            None => cx.notify(),
        }
    }

    fn toggle_selected_drawing_lock(&mut self, cx: &mut Context<Self>) {
        self.edit_selected_drawing(cx, |chart, settings| {
            chart.set_selected_drawing_locked(!settings.flag("locked"));
        });
    }

    fn hide_selected_drawing(&mut self, cx: &mut Context<Self>) {
        self.edit_selected_drawing(cx, |chart, _| {
            chart.set_selected_drawings_visible(false);
        });
    }

    fn clone_selected_drawing(&mut self, cx: &mut Context<Self>) {
        self.edit_selected_drawing(cx, |chart, _| {
            chart.clone_selected_drawings();
        });
    }

    fn remove_selected_drawing(&mut self, cx: &mut Context<Self>) {
        self.edit_selected_drawing(cx, |chart, _| {
            chart.remove_selected_drawing();
        });
    }

    /// Runs one drawing action after closing anything the toolbar had open.
    fn edit_selected_drawing(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut AerisChartView, &ChartDrawingSettings),
    ) {
        self.end_drawing_toolbar_color_edit(cx);
        self.close_drawing_toolbar_menu(cx);
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        selected.chart.update(cx, |chart, chart_cx| {
            edit(chart, &selected.settings);
            chart_cx.notify();
        });
        cx.notify();
    }

    fn open_selected_drawing_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_drawing_toolbar_color_edit(cx);
        self.close_drawing_toolbar_menu(cx);
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        selected.surface.update(cx, |surface, surface_cx| {
            surface.open_drawing_settings_dialog(selected.settings.id, window, surface_cx);
        });
        cx.notify();
    }

    fn request_selected_drawing_order(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_drawing_toolbar_color_edit(cx);
        self.close_drawing_toolbar_menu(cx);
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        let long = selected.settings.kind() == ChartDrawingKind::LongPosition;
        self.request_drawing_bracket_order(selected.chart, selected.settings.id, long, window, cx);
    }

    fn open_drawing_template_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Template name"));
        input.update(cx, |input, input_cx| input.focus(window, input_cx));
        let terminal = cx.entity().downgrade();
        let submit = window.subscribe(&input, cx, move |_, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. })
                && let Err(error) = terminal.update(cx, Self::save_drawing_template)
                && terminal.upgrade().is_some()
            {
                diagnostic!("Live workspace could not save drawing template: {error}");
            }
        });
        let toolbar = self.chrome.context_toolbar_mut();
        toolbar.template_name = Some(TemplateNameField {
            input,
            _submit: submit,
        });
        toolbar.template_error = None;
        cx.notify();
    }

    /// Saves the selected drawing's style under the typed name, replacing a template of the
    /// same kind and name.
    fn save_drawing_template(&mut self, cx: &mut Context<Self>) {
        let Some(field) = &self.chrome.context_toolbar().template_name else {
            return;
        };
        let name = field.input.read(cx).value().trim().to_string();
        let error = self.store_drawing_template(&name, cx).err();
        let toolbar = self.chrome.context_toolbar_mut();
        if error.is_none() {
            toolbar.template_name = None;
            toolbar.menu = None;
        }
        toolbar.template_error = error;
        cx.notify();
    }

    fn store_drawing_template(&mut self, name: &str, cx: &App) -> Result<(), String> {
        if !valid_template_name(name) {
            return Err("Use a name between 1 and 64 characters.".to_string());
        }
        let selected = self.toolbar_drawing(cx).ok_or("Select a drawing first.")?;
        let template_json = selected
            .chart
            .read(cx)
            .drawing_template_json(selected.settings.id, name)
            .filter(|json| json.len() <= MAX_DRAWING_TEMPLATE_BYTES)
            .ok_or("This drawing's style cannot be saved as a template.")?;
        let kind = selected.settings.kind().name().to_string();
        let template = WorkspaceDrawingTemplateState {
            kind,
            name: name.to_string(),
            template_json,
        };
        if !self.saved.store_drawing_template(template) {
            return Err(format!(
                "You can save up to {MAXIMUM_DRAWING_TEMPLATES} drawing templates."
            ));
        }
        self.persist_workspace_layout_if_changed(cx);
        Ok(())
    }

    fn apply_drawing_template(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(json) = self
            .saved
            .drawing_templates()
            .get(index)
            .map(|template| template.template_json.clone())
        else {
            return;
        };
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        let applied = selected.chart.update(cx, |chart, chart_cx| {
            let applied = chart.apply_drawing_template_json(selected.settings.id, &json);
            chart_cx.notify();
            applied
        });
        let toolbar = self.chrome.context_toolbar_mut();
        if applied {
            toolbar.menu = None;
            toolbar.template_error = None;
        } else {
            toolbar.template_error = Some("This template no longer fits this drawing.".to_string());
        }
        cx.notify();
    }

    fn remove_drawing_template(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.saved.remove_drawing_template(index) {
            self.persist_workspace_layout_if_changed(cx);
            cx.notify();
        }
    }

    fn begin_drawing_context_toolbar_move(
        &mut self,
        pointer: gpui::Point<Pixels>,
        origin: gpui::Point<Pixels>,
    ) {
        self.chrome.context_toolbar_mut().grab = Some(pointer - origin);
    }

    /// Follows the pointer in memory only; the position is saved once when the drag ends.
    fn move_drawing_context_toolbar(
        &mut self,
        pointer: gpui::Point<Pixels>,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(grab) = self.chrome.context_toolbar().grab else {
            return;
        };
        let Some(selected) = self.toolbar_drawing(cx) else {
            return;
        };
        let toolbar = toolbar_size(&selected.settings, self.theme.dimensions.border_width);
        let origin = clamp_floating_toolbar_origin(pointer - grab, viewport, toolbar);
        let origin = Some((f32::from(origin.x), f32::from(origin.y)));
        if self.chrome.preferences().drawing_context_toolbar_origin != origin {
            self.chrome.preferences_mut().drawing_context_toolbar_origin = origin;
            cx.notify();
        }
    }

    /// Every surface saves its own copy of the chrome preferences, so each one takes the new
    /// position before the shared file is written.
    pub(super) fn end_drawing_context_toolbar_move(&mut self, cx: &mut Context<Self>) {
        if self.chrome.context_toolbar_mut().grab.take().is_none() {
            return;
        }
        let origin = self.chrome.preferences().drawing_context_toolbar_origin;
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.drawing_context_toolbar_origin = origin;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }

    /// The toolbar of the active chart's selected drawing; `chart_top` is where the workspace
    /// content begins.
    pub(super) fn drawing_context_overlay(
        &self,
        terminal: &Entity<Self>,
        chart_top: f32,
        window: &Window,
        cx: &App,
    ) -> Option<AnyElement> {
        let selected = self.toolbar_drawing(cx)?;
        let theme = &self.theme;
        let settings = &selected.settings;
        let viewport = window.viewport_size();
        let panel_size = toolbar_size(settings, theme.dimensions.border_width);
        let stored = self
            .chrome
            .preferences()
            .drawing_context_toolbar_origin
            .map_or_else(
                || default_origin(chart_top, viewport, panel_size),
                |(x, y)| point(px(x), px(y)),
            );
        let origin = clamp_floating_toolbar_origin(stored, viewport, panel_size);
        let ui = ToolbarUi {
            terminal,
            toolbar: self.chrome.context_toolbar(),
            settings,
            templates: self.saved.drawing_templates(),
            theme,
        };
        let controls = toolbar_controls(settings)
            .into_iter()
            .enumerate()
            .map(|(slot, control)| match control {
                ToolbarControl::Color(property) => color_control(ui, slot, property),
                ToolbarControl::Width(property) => width_control(ui, property),
                ToolbarControl::Choice(property) => choice_control(ui, property),
            });
        let move_terminal = terminal.clone();
        let grab = terminal.clone();
        Some(
            floating_toolbar_panel("drawing_context_toolbar", origin, panel_size, theme)
                .on_drag_move::<DrawingContextMoveDrag>(move |event, window, cx| {
                    let viewport = window.viewport_size();
                    move_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.move_drawing_context_toolbar(
                            event.event.position,
                            viewport,
                            terminal_cx,
                        );
                    });
                })
                .child(floating_toolbar_grip(
                    "drawing_context_grip",
                    "Move drawing toolbar",
                    DrawingContextMoveDrag,
                    theme,
                    move |pointer, cx| {
                        grab.update(cx, |terminal, _| {
                            terminal.begin_drawing_context_toolbar_move(pointer, origin);
                        });
                    },
                ))
                .child(templates_control(ui))
                .children(controls)
                .child(action_button(
                    ui,
                    "drawing_context_settings",
                    HugeIcon::Settings,
                    "Settings",
                    TerminalApp::open_selected_drawing_settings,
                ))
                .child(lock_button(ui))
                .child(
                    action_button(
                        ui,
                        "drawing_context_remove",
                        HugeIcon::Trash,
                        "Remove",
                        |terminal, _, cx| terminal.remove_selected_drawing(cx),
                    )
                    .danger_on_hover(),
                )
                .child(more_control(ui))
                .children(is_position(settings.kind()).then(|| order_button(ui)))
                .into_any_element(),
        )
    }
}

/// What every toolbar control reads.
#[derive(Clone, Copy)]
struct ToolbarUi<'a> {
    terminal: &'a Entity<TerminalApp>,
    toolbar: &'a DrawingContextToolbar,
    settings: &'a ChartDrawingSettings,
    templates: &'a [WorkspaceDrawingTemplateState],
    theme: &'a AerisTheme,
}

impl ToolbarUi<'_> {
    fn menu_open(&self, menu: &ToolbarMenu) -> bool {
        self.toolbar.menu.as_ref() == Some(menu)
    }
}

fn toolbar_button(id: impl Into<gpui::ElementId>, label: &str, theme: &AerisTheme) -> Button {
    Button::new(id, theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Default)
        .aria_label(label.to_string())
        .tooltip(TooltipSpec::new(label.to_string(), theme).show_delay(TOOLTIP_OPEN_DELAY))
}

fn action_button(
    ui: ToolbarUi<'_>,
    id: &'static str,
    icon: HugeIcon,
    label: &'static str,
    action: impl Fn(&mut TerminalApp, &mut Window, &mut Context<TerminalApp>) + 'static,
) -> Button {
    let terminal = ui.terminal.clone();
    toolbar_button(id, label, ui.theme)
        .icon(header_icon(icon))
        .on_press(move |_, window, cx| {
            terminal.update(cx, |terminal, terminal_cx| {
                action(terminal, window, terminal_cx);
            });
        })
}

fn lock_button(ui: ToolbarUi<'_>) -> Button {
    let locked = ui.settings.flag("locked");
    let (icon, label) = if locked {
        (HugeIcon::Lock, "Unlock")
    } else {
        (HugeIcon::Unlock, "Lock")
    };
    action_button(
        ui,
        "drawing_context_lock",
        icon,
        label,
        |terminal, _, cx| {
            terminal.toggle_selected_drawing_lock(cx);
        },
    )
    .selected(locked)
}

/// A toolbar trigger and, while open, its menu below it.
fn toolbar_menu(
    ui: ToolbarUi<'_>,
    id: &'static str,
    trigger: Button,
    menu: Option<MenuPanel>,
) -> MenuAnchor {
    let dismiss = ui.terminal.clone();
    MenuAnchor::new(id, trigger)
        .menu(menu)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, TerminalApp::close_drawing_toolbar_menu);
        })
}

fn menu_panel(id: &'static str, width: f32, theme: &AerisTheme) -> MenuPanel {
    MenuPanel::new(
        id,
        MenuPlacement::Anchored {
            side: MenuSide::Below,
            align: MenuAlign::Start,
        },
        theme,
    )
    .width(px(width))
    .animate_from(PopupAnimationOrigin::TOP_LEFT)
}

fn menu_trigger(ui: ToolbarUi<'_>, id: &'static str, label: &str, menu: ToolbarMenu) -> Button {
    let open = ui.menu_open(&menu);
    let toggle = ui.terminal.clone();
    toolbar_button(id, label, ui.theme)
        .open(open)
        .on_press(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_drawing_toolbar_menu(menu.clone(), terminal_cx);
            });
        })
}

/// A color's swatch on the transparency backdrop; a text color is the drawing sidebar's Text
/// tool glyph over a bar of the color.
fn color_glyph(property: &ChartDrawingProperty, color: &str, theme: &AerisTheme) -> Div {
    if property.group != aeris_chart_integration::ChartDrawingPropertyGroup::Text {
        return color_swatch_dot(color, theme);
    }
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(1.0))
        .child(Icon::new(assets::DrawingIcon::Text.path()).with_size(px(TEXT_GLYPH_SIZE)))
        .child(
            div()
                .w(px(14.0))
                .h(px(3.0))
                .rounded_full()
                .bg(chart_css_color(color, theme.colors.text_secondary)),
        )
}

fn color_control(ui: ToolbarUi<'_>, slot: usize, property: &ChartDrawingProperty) -> AnyElement {
    let theme = ui.theme;
    let name = property.name.clone();
    let shown = ui
        .settings
        .color(&name)
        .unwrap_or_else(|| "Auto".to_string());
    let open_picker = ui
        .toolbar
        .color_picker
        .as_ref()
        .filter(|picker| picker.property == name);
    let toggle = ui.terminal.clone();
    let trigger = toolbar_button(("drawing_context_color", slot), &property.label, theme)
        .leading(color_glyph(property, &shown, theme))
        .open(open_picker.is_some())
        .on_press(move |_, window, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_drawing_toolbar_color(&name, window, terminal_cx);
            });
        });
    let mut control = div().relative().flex_none().child(trigger);
    if let Some(picker) = open_picker {
        let select = ui.terminal.clone();
        let dismiss = ui.terminal.clone();
        control = control.child(gpui::deferred(
            ColorPicker::new(
                ("drawing_context_picker", slot),
                shown,
                &picker.input,
                theme,
            )
            .error(picker.error.as_deref())
            .on_select(move |value, _, cx| {
                select.update(cx, |terminal, terminal_cx| {
                    terminal.set_drawing_toolbar_color(&value, terminal_cx);
                });
            })
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, TerminalApp::close_drawing_toolbar_color);
            }),
        ));
    }
    control.into_any_element()
}

/// A horizontal stroke of `width` logical pixels, the glyph of a width choice.
pub(super) fn width_glyph(width: f64, theme: &AerisTheme) -> Div {
    let height = [4.0_f32, 3.0, 2.0]
        .into_iter()
        .find(|height| f64::from(*height) <= width)
        .unwrap_or(1.0);
    div()
        .w(px(16.0))
        .h(px(height))
        .rounded_full()
        .bg(gpui_color(theme.colors.text_secondary))
}

fn width_control(ui: ToolbarUi<'_>, property: &ChartDrawingProperty) -> AnyElement {
    let theme = ui.theme;
    let current = ui.settings.number(&property.name);
    let menu = ToolbarMenu::Property(property.name.clone());
    let trigger = menu_trigger(ui, "drawing_context_width", &property.label, menu.clone()).leading(
        div()
            .text_xs()
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(current.map_or_else(|| property.label.clone(), width_label)),
    );
    let panel = ui.menu_open(&menu).then(|| {
        menu_panel("drawing_context_width_menu", TOOLBAR_MENU_WIDTH, theme).children(
            width_choices(property).enumerate().map(|(index, width)| {
                let choose = ui.terminal.clone();
                let name = property.name.clone();
                MenuRow::compact(
                    ("drawing_context_width_choice", index),
                    width_label(width),
                    theme,
                )
                .leading(width_glyph(width, theme))
                .checked(current == Some(width))
                .on_click(move |_, _, cx| {
                    choose.update(cx, |terminal, terminal_cx| {
                        terminal.choose_drawing_toolbar_option(
                            &name,
                            Value::from(width),
                            terminal_cx,
                        );
                    });
                })
            }),
        )
    });
    toolbar_menu(ui, "drawing_context_width_anchor", trigger, panel).into_any_element()
}

/// Line styles read as a stroke preview; other choices as their label.
pub(super) fn choice_glyph(choice: &str, theme: &AerisTheme) -> AnyElement {
    let ink = gpui_color(theme.colors.text_secondary);
    let segments = |count: usize, length: f32, gap: f32| {
        div()
            .w(px(16.0))
            .flex()
            .items_center()
            .gap(px(gap))
            .children((0..count).map(move |_| div().w(px(length)).h(px(2.0)).bg(ink)))
            .into_any_element()
    };
    match choice {
        "solid" => segments(1, 16.0, 0.0),
        "dashed" => segments(3, 4.0, 2.0),
        "dotted" => segments(5, 2.0, 1.5),
        _ => div()
            .text_xs()
            .text_color(ink)
            .child(humanize(choice))
            .into_any_element(),
    }
}

fn choice_control(ui: ToolbarUi<'_>, property: &ChartDrawingProperty) -> AnyElement {
    let theme = ui.theme;
    let current = ui.settings.text(&property.name).to_string();
    let menu = ToolbarMenu::Property(property.name.clone());
    let trigger = menu_trigger(ui, "drawing_context_choice", &property.label, menu.clone())
        .leading(choice_glyph(&current, theme));
    let panel = ui.menu_open(&menu).then(|| {
        menu_panel("drawing_context_choice_menu", TOOLBAR_MENU_WIDTH, theme).children(
            property
                .enum_values
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    let choose = ui.terminal.clone();
                    let name = property.name.clone();
                    let value = choice.clone();
                    MenuRow::compact(
                        ("drawing_context_choice_option", index),
                        humanize(choice),
                        theme,
                    )
                    .leading(choice_glyph(choice, theme))
                    .checked(current == *choice)
                    .on_click(move |_, _, cx| {
                        choose.update(cx, |terminal, terminal_cx| {
                            terminal.choose_drawing_toolbar_option(
                                &name,
                                Value::String(value.clone()),
                                terminal_cx,
                            );
                        });
                    })
                }),
        )
    });
    toolbar_menu(ui, "drawing_context_choice_anchor", trigger, panel).into_any_element()
}

fn templates_control(ui: ToolbarUi<'_>) -> AnyElement {
    let trigger = menu_trigger(
        ui,
        "drawing_context_templates",
        "Template",
        ToolbarMenu::Templates,
    )
    .icon(header_icon(HugeIcon::Template));
    let panel = ui
        .menu_open(&ToolbarMenu::Templates)
        .then(|| templates_menu(ui));
    toolbar_menu(ui, "drawing_context_templates_anchor", trigger, panel).into_any_element()
}

/// Save-as on top, then this kind's saved templates: a click applies one, its trash removes it.
fn templates_menu(ui: ToolbarUi<'_>) -> MenuPanel {
    let theme = ui.theme;
    let kind = ui.settings.kind().name();
    let mut panel = menu_panel("drawing_context_templates_menu", TEMPLATE_MENU_WIDTH, theme);
    if let Some(field) = &ui.toolbar.template_name {
        let save = ui.terminal.clone();
        panel = panel.header(
            div()
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .child(
                    div()
                        .flex_1()
                        .h(px(FLOATING_TOOLBAR_BUTTON_SIZE))
                        .flex()
                        .child(Input::new(&field.input).platform(theme).flex_1()),
                )
                .child(
                    Button::new("drawing_context_template_save", theme)
                        .variant(ButtonVariant::Primary)
                        .label("Save")
                        .on_press(move |_, _, cx| {
                            save.update(cx, TerminalApp::save_drawing_template);
                        }),
                ),
        );
    } else {
        let open = ui.terminal.clone();
        panel = panel.child(
            MenuRow::compact(
                "drawing_context_template_save_as",
                "Save drawing template as…",
                theme,
            )
            .on_click(move |_, window, cx| {
                open.update(cx, |terminal, terminal_cx| {
                    terminal.open_drawing_template_name(window, terminal_cx);
                });
            }),
        );
    }
    if let Some(error) = &ui.toolbar.template_error {
        panel = panel.child(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(gpui_color(theme.colors.danger))
                .child(error.clone()),
        );
    }
    let saved: Vec<_> = ui
        .templates
        .iter()
        .enumerate()
        .filter(|(_, template)| template.kind == kind)
        .collect();
    if saved.is_empty() {
        return panel;
    }
    panel = panel.separator();
    panel.children(saved.into_iter().map(|(index, template)| {
        let apply = ui.terminal.clone();
        let remove = ui.terminal.clone();
        MenuRow::compact(
            ("drawing_context_template", index),
            template.name.clone(),
            theme,
        )
        .trailing(
            Button::new(("drawing_context_template_remove", index), theme)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Sm)
                .icon(header_icon(HugeIcon::Trash))
                .aria_label("Remove template")
                .danger_on_hover()
                .on_press(move |_, _, cx| {
                    remove.update(cx, |terminal, terminal_cx| {
                        terminal.remove_drawing_template(index, terminal_cx);
                    });
                }),
        )
        .on_click(move |_, _, cx| {
            apply.update(cx, |terminal, terminal_cx| {
                terminal.apply_drawing_template(index, terminal_cx);
            });
        })
    }))
}

fn more_control(ui: ToolbarUi<'_>) -> AnyElement {
    let theme = ui.theme;
    let trigger = menu_trigger(ui, "drawing_context_more", "More", ToolbarMenu::More)
        .icon(header_icon(HugeIcon::More));
    let panel = ui.menu_open(&ToolbarMenu::More).then(|| {
        let clone = ui.terminal.clone();
        let hide = ui.terminal.clone();
        menu_panel("drawing_context_more_menu", TOOLBAR_MENU_WIDTH, theme)
            .child(
                MenuRow::compact("drawing_context_clone", "Clone", theme)
                    .leading_icon(header_icon(HugeIcon::Copy))
                    .on_click(move |_, _, cx| {
                        clone.update(cx, TerminalApp::clone_selected_drawing);
                    }),
            )
            .child(
                MenuRow::compact("drawing_context_hide", "Hide", theme)
                    .leading_icon(header_icon(HugeIcon::ViewOff))
                    .on_click(move |_, _, cx| {
                        hide.update(cx, TerminalApp::hide_selected_drawing);
                    }),
            )
    });
    toolbar_menu(ui, "drawing_context_more_anchor", trigger, panel).into_any_element()
}

/// Places the bracket the position drawing describes, after the trading confirmation.
fn order_button(ui: ToolbarUi<'_>) -> AnyElement {
    let terminal = ui.terminal.clone();
    div()
        .flex_none()
        .w(px(ORDER_BUTTON_WIDTH))
        .flex()
        .child(
            Button::new("drawing_context_order", ui.theme)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Default)
                .full_width()
                .label("Create order")
                .on_press(move |_, window, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.request_selected_drawing_order(window, terminal_cx);
                    });
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_chart_integration::ChartDrawingPropertyGroup as Group;

    fn property(
        name: &str,
        property_type: Type,
        min: Option<f64>,
        max: Option<f64>,
    ) -> ChartDrawingProperty {
        ChartDrawingProperty {
            name: name.to_string(),
            label: name.to_string(),
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
    fn width_menus_offer_only_widths_inside_the_property_bounds() {
        let bounded = property("width", Type::Number, Some(2.0), Some(3.0));
        assert_eq!(width_choices(&bounded).collect::<Vec<_>>(), [2.0, 3.0]);
        let open = property("width", Type::Number, None, None);
        assert_eq!(width_choices(&open).count(), LINE_WIDTHS.len());
    }

    #[test]
    fn template_names_follow_the_workspace_sanitizer() {
        assert!(valid_template_name("Orange trend"));
        assert!(!valid_template_name(""));
        assert!(!valid_template_name("tab\tname"));
        assert!(!valid_template_name(
            &"x".repeat(MAXIMUM_DRAWING_TEMPLATE_NAME_BYTES + 1)
        ));
    }

    #[test]
    fn a_toolbar_patch_changes_exactly_one_option() {
        assert_eq!(
            single_option("width", Value::from(2.0)),
            serde_json::json!({ "width": 2.0 })
        );
    }

    #[test]
    fn the_toolbar_opens_centered_below_the_chart_header() {
        let origin = default_origin(40.0, size(px(1000.0), px(800.0)), size(px(300.0), px(32.0)));
        assert_eq!(
            origin,
            point(px(350.0), px(40.0 + FLOATING_TOOLBAR_DEFAULT_INSET))
        );
    }
}
