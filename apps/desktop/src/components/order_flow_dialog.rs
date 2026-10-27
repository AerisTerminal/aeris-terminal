//! Settings dialogs of the order-flow indicators: cumulative delta (CVD), the delta histogram
//! and the order book on the chart. Each dialog edits one indicator's settings and previews every
//! valid edit through the chart's order-flow settings owner; Apply keeps what the chart shows and
//! Cancel restores the settings the dialog opened with.

use super::big_trades_dialog::{
    SettingDropdown, control_strip, dropdown_choices, setting_input, setting_row, settings_group,
    settings_groups,
};
use super::*;
use crate::desktop::workspace_surface::preview_on_change;

const STYLE_CHOICES: [(&str, CumulativeDeltaStyle); 2] = [
    ("Line", CumulativeDeltaStyle::Line),
    ("Candles", CumulativeDeltaStyle::Candles),
];

const RESET_CHOICES: [(&str, CumulativeDeltaReset); 2] = [
    ("Each session", CumulativeDeltaReset::Session),
    ("Never", CumulativeDeltaReset::Continuous),
];

/// The order-flow indicator a settings dialog edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrderFlowIndicator {
    CumulativeDelta,
    DeltaHistogram,
    OrderBook,
}

impl OrderFlowIndicator {
    const fn title(self) -> &'static str {
        match self {
            Self::CumulativeDelta => "Cumulative delta (CVD)",
            Self::DeltaHistogram => "Delta histogram",
            Self::OrderBook => "Order book on chart (DOM)",
        }
    }

    const fn removed_message(self) -> &'static str {
        match self {
            Self::CumulativeDelta => "Cumulative delta is no longer on this chart.",
            Self::DeltaHistogram => "The delta histogram is no longer on this chart.",
            Self::OrderBook => "The order book is no longer on this chart.",
        }
    }
}

/// The draft of the one indicator a dialog edits.
pub(super) enum OrderFlowDialogDraft {
    CumulativeDelta(CumulativeDeltaSettings),
    DeltaHistogram(DeltaHistogramSettings),
    OrderBook(OrderBookDraft),
}

impl OrderFlowDialogDraft {
    const fn indicator(&self) -> OrderFlowIndicator {
        match self {
            Self::CumulativeDelta(_) => OrderFlowIndicator::CumulativeDelta,
            Self::DeltaHistogram(_) => OrderFlowIndicator::DeltaHistogram,
            Self::OrderBook(_) => OrderFlowIndicator::OrderBook,
        }
    }

    /// The draft's setting for `target`; `None` both when it is unset and when this draft has
    /// no such color.
    const fn color(&self, target: OrderFlowColorTarget) -> Option<ChartColor> {
        match (self, target) {
            (Self::CumulativeDelta(draft), OrderFlowColorTarget::CumulativeDeltaLine) => {
                draft.line_color
            }
            (Self::CumulativeDelta(draft), OrderFlowColorTarget::CumulativeDeltaUp) => {
                draft.up_color
            }
            (Self::CumulativeDelta(draft), OrderFlowColorTarget::CumulativeDeltaDown) => {
                draft.down_color
            }
            (Self::DeltaHistogram(draft), OrderFlowColorTarget::DeltaPositive) => draft.up_color,
            (Self::DeltaHistogram(draft), OrderFlowColorTarget::DeltaNegative) => draft.down_color,
            (Self::OrderBook(draft), OrderFlowColorTarget::Bid) => draft.settings.bid_color,
            (Self::OrderBook(draft), OrderFlowColorTarget::Ask) => draft.settings.ask_color,
            _ => None,
        }
    }

    const fn color_slot(
        &mut self,
        target: OrderFlowColorTarget,
    ) -> Option<&mut Option<ChartColor>> {
        match (self, target) {
            (Self::CumulativeDelta(draft), OrderFlowColorTarget::CumulativeDeltaLine) => {
                Some(&mut draft.line_color)
            }
            (Self::CumulativeDelta(draft), OrderFlowColorTarget::CumulativeDeltaUp) => {
                Some(&mut draft.up_color)
            }
            (Self::CumulativeDelta(draft), OrderFlowColorTarget::CumulativeDeltaDown) => {
                Some(&mut draft.down_color)
            }
            (Self::DeltaHistogram(draft), OrderFlowColorTarget::DeltaPositive) => {
                Some(&mut draft.up_color)
            }
            (Self::DeltaHistogram(draft), OrderFlowColorTarget::DeltaNegative) => {
                Some(&mut draft.down_color)
            }
            (Self::OrderBook(draft), OrderFlowColorTarget::Bid) => {
                Some(&mut draft.settings.bid_color)
            }
            (Self::OrderBook(draft), OrderFlowColorTarget::Ask) => {
                Some(&mut draft.settings.ask_color)
            }
            _ => None,
        }
    }
}

/// Draft order-book settings. The bounded numbers stay text and preview once they are valid.
pub(super) struct OrderBookDraft {
    pub(super) settings: OrderBookDepthSettings,
    pub(super) levels_per_side: Entity<InputState>,
    pub(super) max_width: Entity<InputState>,
    pub(super) opacity_percent: Entity<InputState>,
}

impl OrderBookDraft {
    fn new(
        settings: OrderBookDepthSettings,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) -> Self {
        Self {
            settings,
            levels_per_side: number_input(
                settings.levels_per_side.to_string(),
                "Levels",
                window,
                cx,
            ),
            max_width: number_input(settings.max_width.to_string(), "Pixels", window, cx),
            opacity_percent: number_input(
                settings.opacity_percent.to_string(),
                "Percent",
                window,
                cx,
            ),
        }
    }

    /// Restores the defaults; whether the indicator is shown stays with the legend.
    fn reset(&mut self, window: &mut Window, cx: &mut App) {
        let defaults = OrderBookDepthSettings {
            visible: self.settings.visible,
            ..OrderBookDepthSettings::default()
        };
        self.settings = defaults;
        for (input, value) in [
            (&self.levels_per_side, defaults.levels_per_side.to_string()),
            (&self.max_width, defaults.max_width.to_string()),
            (&self.opacity_percent, defaults.opacity_percent.to_string()),
        ] {
            input.update(cx, |input, input_cx| {
                input.set_value(value, window, input_cx);
            });
        }
    }

    /// The draft as applied over `current`, or why its numbers cannot be applied.
    fn applied(
        &self,
        current: OrderBookDepthSettings,
        cx: &App,
    ) -> Result<OrderBookDepthSettings, String> {
        Ok(OrderBookDepthSettings {
            visible: current.visible,
            levels_per_side: parse_bounded_setting(
                self.levels_per_side.read(cx).value().as_ref(),
                OrderBookDepthSettings::LEVELS_PER_SIDE,
                "Levels per side",
            )?,
            max_width: parse_bounded_setting(
                self.max_width.read(cx).value().as_ref(),
                OrderBookDepthSettings::MAX_WIDTHS,
                "Largest bar",
            )?,
            opacity_percent: parse_bounded_setting(
                self.opacity_percent.read(cx).value().as_ref(),
                OrderBookDepthSettings::OPACITY_PERCENTS,
                "Opacity",
            )?,
            ..self.settings
        })
    }
}

fn number_input(
    value: String,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> Entity<InputState> {
    let input = cx.new(|input_cx| InputState::new(window, input_cx).placeholder(placeholder));
    input.update(cx, |input, input_cx| {
        input.set_value(value, window, input_cx);
    });
    input
}

fn parse_bounded_setting<T>(
    text: &str,
    range: std::ops::RangeInclusive<T>,
    label: &str,
) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display,
{
    text.trim()
        .parse::<T>()
        .ok()
        .filter(|value| range.contains(value))
        .ok_or_else(|| {
            format!(
                "{label} must be a whole number from {} to {}",
                range.start(),
                range.end()
            )
        })
}

/// Settings of one order-flow indicator being edited, previewed on the chart as they change.
pub(super) struct OrderFlowDialogState {
    /// The chart's order-flow settings when the dialog opened; Cancel restores the edited part.
    pub(super) original: OrderFlowSettings,
    pub(super) draft: OrderFlowDialogDraft,
    /// The colors the chart draws where the draft leaves a color unset.
    pub(super) defaults: OrderFlowDefaultColors,
    pub(super) color_picker: Option<DialogColorPickerState<OrderFlowColorTarget>>,
    /// The setting whose dropdown is open.
    pub(super) menu: Option<String>,
    pub(super) message: Option<String>,
    pub(super) _subscriptions: Vec<gpui::Subscription>,
}

impl OrderFlowDialogState {
    /// `target`'s color as drawn once the draft is applied, as `#RRGGBB` or `#RRGGBBAA`.
    pub(super) fn effective_color(&self, target: OrderFlowColorTarget) -> String {
        self.draft
            .color(target)
            .map_or_else(|| self.default_color(target).to_string(), chart_color_hex)
    }

    fn default_color(&self, target: OrderFlowColorTarget) -> &str {
        let defaults = &self.defaults;
        match target {
            OrderFlowColorTarget::CumulativeDeltaLine => &defaults.cumulative_delta_line,
            OrderFlowColorTarget::CumulativeDeltaUp | OrderFlowColorTarget::Bid => {
                &defaults.bullish
            }
            OrderFlowColorTarget::CumulativeDeltaDown | OrderFlowColorTarget::Ask => {
                &defaults.bearish
            }
            OrderFlowColorTarget::DeltaPositive => &defaults.delta_positive,
            OrderFlowColorTarget::DeltaNegative => &defaults.delta_negative,
        }
    }
}

/// The order-flow color a dialog swatch edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrderFlowColorTarget {
    CumulativeDeltaLine,
    CumulativeDeltaUp,
    CumulativeDeltaDown,
    DeltaPositive,
    DeltaNegative,
    Bid,
    Ask,
}

impl OrderFlowColorTarget {
    const fn label(self) -> &'static str {
        match self {
            Self::CumulativeDeltaLine => "Line",
            Self::CumulativeDeltaUp => "Rising candles",
            Self::CumulativeDeltaDown => "Falling candles",
            Self::DeltaPositive => "Positive delta",
            Self::DeltaNegative => "Negative delta",
            Self::Bid => "Bids",
            Self::Ask => "Asks",
        }
    }
}

/// `settings` with the draft applied, or why it cannot be applied.
fn applied_order_flow_settings(
    draft: &OrderFlowDialogDraft,
    current: OrderFlowSettings,
    cx: &App,
) -> Result<OrderFlowSettings, String> {
    let removed = || draft.indicator().removed_message().to_string();
    let mut settings = current;
    match draft {
        OrderFlowDialogDraft::CumulativeDelta(draft) => {
            if !current.show_cumulative_delta {
                return Err(removed());
            }
            settings.cumulative_delta = *draft;
        }
        OrderFlowDialogDraft::DeltaHistogram(draft) => {
            if !current.show_delta_histogram {
                return Err(removed());
            }
            settings.delta_histogram = *draft;
        }
        OrderFlowDialogDraft::OrderBook(draft) => {
            let book = current.order_book_depth.ok_or_else(removed)?;
            settings.order_book_depth = Some(draft.applied(book, cx)?);
        }
    }
    Ok(settings)
}

/// `current` with `indicator`'s settings restored from `original`. Whether it is shown stays with
/// the legend, and an indicator removed meanwhile stays removed.
fn restored_order_flow_settings(
    indicator: OrderFlowIndicator,
    original: OrderFlowSettings,
    current: OrderFlowSettings,
) -> OrderFlowSettings {
    let mut settings = current;
    match indicator {
        OrderFlowIndicator::CumulativeDelta => {
            settings.cumulative_delta = original.cumulative_delta;
        }
        OrderFlowIndicator::DeltaHistogram => {
            settings.delta_histogram = original.delta_histogram;
        }
        OrderFlowIndicator::OrderBook => {
            if let (Some(book), Some(shown)) = (original.order_book_depth, current.order_book_depth)
            {
                settings.order_book_depth = Some(OrderBookDepthSettings {
                    visible: shown.visible,
                    ..book
                });
            }
        }
    }
    settings
}

impl WorkspaceSurface {
    pub(super) const fn order_flow_dialog(&self) -> Option<&OrderFlowDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::OrderFlow(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(super) const fn order_flow_dialog_mut(&mut self) -> Option<&mut OrderFlowDialogState> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::OrderFlow(dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// Opens one order-flow indicator's settings; every valid edit previews on the chart.
    pub(super) fn open_order_flow_dialog(
        &mut self,
        indicator: OrderFlowIndicator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((settings, defaults)) = self.chart.as_ref().map(|chart| {
            let chart = chart.read(cx);
            (
                chart.order_flow_settings(),
                chart.order_flow_default_colors(),
            )
        }) else {
            return;
        };
        let draft = match indicator {
            OrderFlowIndicator::CumulativeDelta => {
                settings
                    .show_cumulative_delta
                    .then_some(OrderFlowDialogDraft::CumulativeDelta(
                        settings.cumulative_delta,
                    ))
            }
            OrderFlowIndicator::DeltaHistogram => {
                settings
                    .show_delta_histogram
                    .then_some(OrderFlowDialogDraft::DeltaHistogram(
                        settings.delta_histogram,
                    ))
            }
            OrderFlowIndicator::OrderBook => settings
                .order_book_depth
                .map(|book| OrderFlowDialogDraft::OrderBook(OrderBookDraft::new(book, window, cx))),
        };
        let Some(draft) = draft else {
            Self::post_indicator_failure(
                "order_flow_settings",
                "Study unavailable",
                indicator.removed_message(),
                cx,
            );
            return;
        };
        let subscriptions = match &draft {
            OrderFlowDialogDraft::OrderBook(book) => [
                &book.levels_per_side,
                &book.max_width,
                &book.opacity_percent,
            ]
            .into_iter()
            .map(|input| preview_on_change(input, window, cx, Self::preview_order_flow_dialog))
            .collect(),
            OrderFlowDialogDraft::CumulativeDelta(_) | OrderFlowDialogDraft::DeltaHistogram(_) => {
                Vec::new()
            }
        };
        self.open_indicator_settings_dialog(
            IndicatorSettingsDialog::OrderFlow(OrderFlowDialogState {
                original: settings,
                draft,
                defaults,
                color_picker: None,
                menu: None,
                message: None,
                _subscriptions: subscriptions,
            }),
            cx,
        );
    }

    /// Cancel: closes the dialog and restores the indicator's settings it opened with.
    pub(super) fn close_order_flow_dialog(&mut self, cx: &mut Context<Self>) {
        let Some((indicator, original)) = self
            .order_flow_dialog()
            .map(|dialog| (dialog.draft.indicator(), dialog.original))
        else {
            return;
        };
        self.indicator_settings_dialog = None;
        if let Some(current) = self.chart_order_flow_settings(cx) {
            let restored = restored_order_flow_settings(indicator, original, current);
            if restored != current {
                self.set_chart_order_flow_settings(restored, cx);
            }
        }
        cx.notify();
    }

    /// Shows the dialog's settings on the chart, or why they cannot be shown.
    fn preview_order_flow_dialog(&mut self, cx: &mut Context<Self>) {
        let result = self.apply_order_flow_draft(cx);
        if let Some(dialog) = self.order_flow_dialog_mut() {
            dialog.message = result.err();
        }
        cx.notify();
    }

    /// Hands the draft to the order-flow settings owner. Visibility stays with the legend.
    fn apply_order_flow_draft(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(dialog) = self.order_flow_dialog() else {
            return Ok(());
        };
        let Some(current) = self.chart_order_flow_settings(cx) else {
            return Err("The chart is unavailable".to_string());
        };
        let settings = applied_order_flow_settings(&dialog.draft, current, cx)?;
        if settings == current || self.set_chart_order_flow_settings(settings, cx) {
            Ok(())
        } else {
            Err("The chart could not apply these settings".to_string())
        }
    }

    pub(super) fn set_order_flow_dialog_style(
        &mut self,
        style: CumulativeDeltaStyle,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.order_flow_dialog_mut()
            && let OrderFlowDialogDraft::CumulativeDelta(draft) = &mut dialog.draft
            && draft.style != style
        {
            draft.style = style;
            // The style swaps the line swatch for the candle swatches.
            dialog.color_picker = None;
            self.preview_order_flow_dialog(cx);
        }
    }

    pub(super) fn set_order_flow_dialog_reset(
        &mut self,
        reset: CumulativeDeltaReset,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.order_flow_dialog_mut()
            && let OrderFlowDialogDraft::CumulativeDelta(draft) = &mut dialog.draft
            && draft.reset != reset
        {
            draft.reset = reset;
            self.preview_order_flow_dialog(cx);
        }
    }

    pub(super) fn set_order_flow_dialog_show_sizes(
        &mut self,
        show_sizes: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.order_flow_dialog_mut()
            && let OrderFlowDialogDraft::OrderBook(draft) = &mut dialog.draft
            && draft.settings.show_sizes != show_sizes
        {
            draft.settings.show_sizes = show_sizes;
            self.preview_order_flow_dialog(cx);
        }
    }

    pub(super) fn toggle_order_flow_dialog_color_picker(
        &mut self,
        target: OrderFlowColorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.order_flow_dialog_mut() else {
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
            let current = dialog.effective_color(target);
            let input = cx.new(|input_cx| InputState::new(window, input_cx));
            input.update(cx, |input, input_cx| {
                input.set_value(current.to_ascii_uppercase(), window, input_cx);
            });
            dialog.color_picker = Some(DialogColorPickerState {
                target,
                input,
                error: None,
            });
        }
        cx.notify();
    }

    pub(super) fn close_order_flow_dialog_color_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.order_flow_dialog_mut()
            && dialog.color_picker.take().is_some()
        {
            cx.notify();
        }
    }

    pub(super) fn set_order_flow_dialog_color(
        &mut self,
        target: OrderFlowColorTarget,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.order_flow_dialog_mut() else {
            return;
        };
        let parsed = normalize_hex_color(color).and_then(|hex| ChartColor::from_hex(&hex));
        if let Some(picker) = &mut dialog.color_picker
            && picker.target == target
        {
            picker.error = parsed
                .is_none()
                .then(|| "Enter a hex color such as #2962FF or #2962FF80.".to_string());
        }
        if let Some(color) = parsed
            && let Some(slot) = dialog.draft.color_slot(target)
        {
            *slot = Some(color);
            return self.preview_order_flow_dialog(cx);
        }
        cx.notify();
    }

    /// Resets the indicator to its defaults, shown on the chart at once.
    pub(super) fn reset_order_flow_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.order_flow_dialog_mut() else {
            return;
        };
        match &mut dialog.draft {
            OrderFlowDialogDraft::CumulativeDelta(draft) => {
                *draft = CumulativeDeltaSettings::default();
            }
            OrderFlowDialogDraft::DeltaHistogram(draft) => {
                *draft = DeltaHistogramSettings::default();
            }
            OrderFlowDialogDraft::OrderBook(draft) => draft.reset(window, cx),
        }
        dialog.color_picker = None;
        self.preview_order_flow_dialog(cx);
    }

    /// Apply: keeps the settings the chart already shows, closing only when they are valid.
    pub(super) fn apply_order_flow_dialog(&mut self, cx: &mut Context<Self>) {
        match self.apply_order_flow_draft(cx) {
            Ok(()) => {
                self.indicator_settings_dialog = None;
                cx.notify();
            }
            Err(message) => self.show_order_flow_message(message, cx),
        }
    }

    fn show_order_flow_message(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.order_flow_dialog_mut() {
            dialog.message = Some(message);
        }
        cx.notify();
    }
}

/// A swatch that opens the color picker for one order-flow color. Picks preview at once. A
/// `named` swatch shows the color's name instead of its hex code, for paired colors.
fn color_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &OrderFlowDialogState,
    target: OrderFlowColorTarget,
    named: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let value = dialog.effective_color(target);
    let open_picker = dialog
        .color_picker
        .as_ref()
        .filter(|picker| picker.target == target);
    let toggle = app.clone();
    let id = ("order_flow_color", target as usize);
    let button = if named {
        named_color_button(id, target.label(), &value, open_picker.is_some(), theme)
    } else {
        color_swatch_button(id, target.label(), &value, open_picker.is_some(), theme)
    };
    let mut control = div()
        .relative()
        .flex()
        .child(button.on_press(move |_, window, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_order_flow_dialog_color_picker(target, window, surface_cx);
            });
        }));
    if let Some(picker) = open_picker {
        let select = app.clone();
        let dismiss = app.clone();
        control = control.child(gpui::deferred(
            ColorPicker::new(
                ("order_flow_color_popover", target as usize),
                value,
                &picker.input,
                theme,
            )
            .error(picker.error.as_deref())
            .on_select(move |value, _, cx| {
                select.update(cx, |surface, surface_cx| {
                    surface.set_order_flow_dialog_color(target, &value, surface_cx);
                });
            })
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, WorkspaceSurface::close_order_flow_dialog_color_picker);
            }),
        ));
    }
    control.into_any_element()
}

/// One row of swatches side by side, one per target; several targets show their names.
fn colors_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &OrderFlowDialogState,
    label: &'static str,
    description: &'static str,
    targets: &[OrderFlowColorTarget],
    theme: &AerisTheme,
) -> AnyElement {
    let named = targets.len() > 1;
    setting_row(
        label,
        description,
        control_strip(
            targets
                .iter()
                .map(|&target| color_control(app, dialog, target, named, theme)),
        ),
        theme,
    )
}

fn number_row(
    label: &'static str,
    description: &'static str,
    input: &Entity<InputState>,
    theme: &AerisTheme,
) -> AnyElement {
    setting_row(label, description, setting_input(input, theme), theme)
}

fn cumulative_delta_groups(
    app: &Entity<WorkspaceSurface>,
    dialog: &OrderFlowDialogState,
    draft: &CumulativeDeltaSettings,
    theme: &AerisTheme,
) -> [AnyElement; 2] {
    let style = SettingDropdown {
        menu: "cumulative_delta_style",
        label: "Style",
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: dropdown_choices(&STYLE_CHOICES, |style| draft.style == style),
    }
    .render(app, WorkspaceSurface::set_order_flow_dialog_style, theme);
    let reset = SettingDropdown {
        menu: "cumulative_delta_reset",
        label: "Reset",
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: dropdown_choices(&RESET_CHOICES, |reset| draft.reset == reset),
    }
    .render(app, WorkspaceSurface::set_order_flow_dialog_reset, theme);
    let reset_description = match draft.reset {
        CumulativeDeltaReset::Session => "The delta starts again from zero at each session open.",
        CumulativeDeltaReset::Continuous => "The delta keeps adding up across every bar.",
    };
    let colors = match draft.style {
        CumulativeDeltaStyle::Line => colors_row(
            app,
            dialog,
            "Line color",
            "Color of the running-delta line.",
            &[OrderFlowColorTarget::CumulativeDeltaLine],
            theme,
        ),
        CumulativeDeltaStyle::Candles => colors_row(
            app,
            dialog,
            "Candle colors",
            "Each candle takes the color of the way the delta moved in its bar.",
            &[
                OrderFlowColorTarget::CumulativeDeltaUp,
                OrderFlowColorTarget::CumulativeDeltaDown,
            ],
            theme,
        ),
    };
    [
        settings_group(
            "Calculation",
            [
                setting_row(
                    "Style",
                    "A line through each bar's close, or candles of the delta within each bar.",
                    style,
                    theme,
                ),
                setting_row("Reset", reset_description, reset, theme),
            ],
            theme,
        ),
        settings_group("Colors", [colors], theme),
    ]
}

fn order_book_groups(
    app: &Entity<WorkspaceSurface>,
    dialog: &OrderFlowDialogState,
    draft: &OrderBookDraft,
    theme: &AerisTheme,
) -> [AnyElement; 2] {
    let update = app.clone();
    let show_sizes = div()
        .py_1()
        .child(
            SwitchRow::new(
                "order_book_show_sizes",
                "Show sizes",
                draft.settings.show_sizes,
                theme,
            )
            .description("Write each level's resting size on its bar.")
            .on_change(move |show_sizes, _, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_order_flow_dialog_show_sizes(show_sizes, surface_cx);
                });
            }),
        )
        .into_any_element();
    [
        settings_group(
            "Bars",
            [
                number_row(
                    "Levels per side",
                    "Book levels drawn above and below the price, from 1 to 200.",
                    &draft.levels_per_side,
                    theme,
                ),
                number_row(
                    "Largest bar",
                    "Length in pixels of the largest level's bar, from 20 to 400.",
                    &draft.max_width,
                    theme,
                ),
                number_row(
                    "Opacity",
                    "Bar opacity in percent, from 10 to 100.",
                    &draft.opacity_percent,
                    theme,
                ),
            ],
            theme,
        ),
        settings_group(
            "Display",
            [
                show_sizes,
                colors_row(
                    app,
                    dialog,
                    "Colors",
                    "Bid bars sit below the price and ask bars above it.",
                    &[OrderFlowColorTarget::Bid, OrderFlowColorTarget::Ask],
                    theme,
                ),
            ],
            theme,
        ),
    ]
}

fn order_flow_dialog_groups(
    app: &Entity<WorkspaceSurface>,
    dialog: &OrderFlowDialogState,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    match &dialog.draft {
        OrderFlowDialogDraft::CumulativeDelta(draft) => {
            cumulative_delta_groups(app, dialog, draft, theme).into()
        }
        OrderFlowDialogDraft::DeltaHistogram(_) => vec![settings_group(
            "Columns",
            [colors_row(
                app,
                dialog,
                "Colors",
                "Columns above zero take the positive color and columns below it the negative.",
                &[
                    OrderFlowColorTarget::DeltaPositive,
                    OrderFlowColorTarget::DeltaNegative,
                ],
                theme,
            )],
            theme,
        )],
        OrderFlowDialogDraft::OrderBook(draft) => {
            order_book_groups(app, dialog, draft, theme).into()
        }
    }
}

pub(super) fn order_flow_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &OrderFlowDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let reset = app.clone();
    let cancel = app.clone();
    let apply = app.clone();
    Dialog::new("order_flow_dialog", DialogSize::Md, theme, move |_, cx| {
        dismiss.update(cx, WorkspaceSurface::close_order_flow_dialog);
    })
    .title("Indicator settings")
    .subtitle(dialog.draft.indicator().title())
    .max_height(px(680.0))
    .child(settings_groups(
        order_flow_dialog_groups(app, dialog, theme),
        theme,
    ))
    .children(dialog.message.as_ref().map(|message| {
        div()
            .text_sm()
            .text_color(gpui_color(colors.danger))
            .child(message.clone())
    }))
    .footer_leading(
        Button::new("order_flow_reset", theme)
            .variant(ButtonVariant::Destructive)
            .label("Reset to defaults")
            .on_click(move |_, window, cx| {
                reset.update(cx, |surface, surface_cx| {
                    surface.reset_order_flow_dialog(window, surface_cx);
                });
            }),
    )
    .action(
        Button::new("order_flow_cancel", theme)
            .variant(ButtonVariant::Outline)
            .label("Cancel")
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_order_flow_dialog);
            }),
    )
    .action(
        Button::new("order_flow_apply", theme)
            .variant(ButtonVariant::Primary)
            .label("Apply")
            .on_click(move |_, _, cx| {
                apply.update(cx, WorkspaceSurface::apply_order_flow_dialog);
            }),
    )
    .into_any_element()
}
