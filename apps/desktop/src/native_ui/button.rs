//! `Aeris`'s one push/toggle button: the Theme System `Button`
//! (`Theme_System/src/components/ui/showcase.tsx`) in GPUI.
//!
//! Pick a [`ButtonVariant`] and a [`ButtonSize`] by name; the variant owns every colour, border,
//! hover, press, disabled and focus state, exactly as the Theme System defines it. Callers add
//! only layout (`flex_1`, margins, an explicit width, or a height for a strip such as a
//! disclosure arrow); they never restyle a button's colours, borders, radius or hover states.
//!
//! | Variant       | Look                                    | Use for                               |
//! |---------------|-----------------------------------------|---------------------------------------|
//! | `Default`     | `button-fill` neutral fill              | the main neutral action               |
//! | `Secondary`   | brand `primary` blue fill               | the committing action of a form/dialog |
//! | `Outline`     | `surface` + `border`, `text-default`    | Cancel, triggers, steppers, filters    |
//! | `Ghost`       | no fill or border, `text-default`       | toolbar, header and icon actions       |
//! | `Destructive` | `danger` fill                           | irreversible actions                   |
//! | `Buy`/`Sell`  | `buy`/`sell` fill                       | trade orders only                      |
//!
//! Aeris adds states the Theme System page has no example of, all from the same tokens:
//! `selected` (a toggle that is on), `open` (a trigger whose menu is open), `danger_on_hover`
//! (close and delete icons) and `round` (circular icon actions and pill triggers).
//!
//! `gpui-base` owns press/release pairing, focus and keyboard click synthesis. A button registers
//! exactly one activation path: [`Button::on_click`] (release) or [`Button::on_press`] (press,
//! for controls inside overlays whose scrim dismisses on mouse-down).

use std::{rc::Rc, sync::Arc};

use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, Interactivity, IntoElement,
    MouseButton, ParentElement, Pixels, Point, RenderOnce, SharedString, StyleRefinement, Styled,
    Window, div, prelude::*, px,
};
use gpui_base::Button as BaseButton;

use crate::desktop::assets::UiIcon;

use super::{
    icon::Icon,
    loader::Loader,
    platform_font_weight,
    rem_scale::design_rems,
    theme::{gpui_color, platform_border_width},
    tooltip::TooltipSpec,
};

type ClickActivation = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type PressActivation = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;

/// The Theme System `Button` variants, by the same names. See the module table for when to use
/// each one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ButtonVariant {
    /// `default`: neutral `button-fill`, darker on hover and press.
    #[default]
    Default,
    /// `secondary`: the brand `primary` blue, for the action that commits a form or dialog.
    Secondary,
    /// `outline`: `surface` with a `border` outline and `text-default`. Hover and press drop the
    /// outline and fill with `hover-bg` / `active-bg`.
    Outline,
    /// `ghost`: no fill or outline, `text-default`; hover and press fill with `hover-bg` /
    /// `active-bg`.
    Ghost,
    /// `destructive`: the `danger` ramp.
    Destructive,
    /// `buy`: the `buy` trade ramp. Trade orders only.
    Buy,
    /// `sell`: the `sell` trade ramp. Trade orders only.
    Sell,
}

/// The Theme System `Button` sizes. An icon-only button is square at its size's height, which at
/// `Default` is the Theme System `icon` size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ButtonSize {
    /// `sm`: `h-6` (24 px), `px-2`, `text-xs`.
    Sm,
    /// `default`: `h-7` (28 px), `px-2.5`, `text-sm`.
    #[default]
    Default,
    /// `lg`: `h-8` (32 px), `px-3`, `text-sm`, `--radius-default`.
    Lg,
}

/// Geometry is in design rems so a button grows with an enclosing [`RemScale`] panel and renders
/// at its exact logical size everywhere else; glyphs stay pixel-aligned.
///
/// [`RemScale`]: super::rem_scale::RemScale
impl ButtonSize {
    /// Logical height in pixels at the root rem size.
    pub(crate) const fn logical_height(self) -> f32 {
        match self {
            Self::Sm => 24.0,
            Self::Default => 28.0,
            Self::Lg => 32.0,
        }
    }

    const fn logical_padding_x(self) -> f32 {
        match self {
            Self::Sm => 8.0,
            Self::Default => 10.0,
            Self::Lg => 12.0,
        }
    }

    /// Glyph size for an icon that does not carry its own optical size. The Theme System draws
    /// 14 px glyphs on the web; GPUI snaps a glyph and its hover fill to device pixels
    /// separately, so `Sm` and `Default` use 16 px, which keeps the glyph centred at every
    /// supported scale, and `Lg` keeps the chart header's 18 px glyph.
    fn icon(self) -> Pixels {
        px(match self {
            Self::Sm | Self::Default => 16.0,
            Self::Lg => 18.0,
        })
    }

    const fn dense_text(self) -> bool {
        matches!(self, Self::Sm)
    }

    const fn radius(self) -> RadiusToken {
        match self {
            Self::Sm | Self::Default => RadiusToken::Compact,
            Self::Lg => RadiusToken::Default,
        }
    }
}

/// Resolved token colours for one variant.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ButtonAppearance {
    /// `None` leaves the button transparent over whatever it sits on.
    fill: Option<ThemeColor>,
    foreground: ThemeColor,
    /// An outline at rest; every variant keeps a transparent border of the same width so
    /// changing state never shifts the layout.
    border: Option<ThemeColor>,
    /// The outline disappears while hovered, pressed, selected or disabled (`outline`).
    border_rests_only: bool,
    hover_fill: ThemeColor,
    hover_foreground: ThemeColor,
    active_fill: ThemeColor,
    selected_fill: ThemeColor,
    selected_foreground: ThemeColor,
    /// `None` keeps the resting surface: a disabled icon or outline action never changes the
    /// look of the surface it sits on, only its foreground dims.
    disabled_fill: Option<ThemeColor>,
    disabled_foreground: ThemeColor,
}

fn button_appearance(
    theme: &AerisTheme,
    variant: ButtonVariant,
    danger_on_hover: bool,
) -> ButtonAppearance {
    let colors = theme.colors;
    let mut appearance = match variant {
        ButtonVariant::Default => ramp(
            colors.button_fill,
            colors.button_fill_foreground,
            colors.button_fill_hover,
            colors.button_fill_active,
            colors.disabled_bg,
            colors.text_muted,
        ),
        ButtonVariant::Secondary => ramp(
            colors.primary,
            colors.primary_foreground,
            colors.primary_hover,
            colors.primary_active,
            colors.primary_disabled,
            colors.primary_disabled_foreground,
        ),
        ButtonVariant::Outline | ButtonVariant::Ghost => {
            let outline = variant == ButtonVariant::Outline;
            ButtonAppearance {
                fill: outline.then_some(colors.surface),
                foreground: colors.text_default,
                border: outline.then_some(colors.border),
                border_rests_only: outline,
                hover_fill: colors.hover_bg,
                hover_foreground: colors.text_default,
                active_fill: colors.active_bg,
                selected_fill: colors.active_bg,
                selected_foreground: colors.text_active,
                // Disabled keeps the resting surface and outline; only the foreground mutes.
                disabled_fill: None,
                disabled_foreground: colors.text_muted,
            }
        }
        ButtonVariant::Destructive => ramp(
            colors.danger,
            colors.danger_foreground,
            colors.danger_hover,
            colors.danger_active,
            colors.danger_disabled,
            colors.danger_disabled_foreground,
        ),
        ButtonVariant::Buy => ramp(
            colors.buy,
            colors.buy_foreground,
            colors.buy_hover,
            colors.buy_active,
            colors.buy_disabled,
            colors.buy_disabled_foreground,
        ),
        ButtonVariant::Sell => ramp(
            colors.sell,
            colors.sell_foreground,
            colors.sell_hover,
            colors.sell_active,
            colors.sell_disabled,
            colors.sell_disabled_foreground,
        ),
    };
    if danger_on_hover {
        appearance.hover_fill = colors.danger;
        appearance.hover_foreground = colors.danger_foreground;
        appearance.active_fill = colors.danger_active;
    }
    appearance
}

/// The interactive text tokens a text toggle uses, so its on state differs from its off state
/// by the text and icon colour alone.
const fn text_toggle_appearance(
    mut appearance: ButtonAppearance,
    theme: &AerisTheme,
) -> ButtonAppearance {
    let colors = theme.colors;
    appearance.foreground = colors.text_interactive;
    appearance.hover_foreground = colors.text_hover;
    appearance.selected_foreground = colors.text_active;
    appearance
}

/// A filled variant: one token ramp for rest, hover, press and disabled.
const fn ramp(
    fill: ThemeColor,
    foreground: ThemeColor,
    hover: ThemeColor,
    active: ThemeColor,
    disabled: ThemeColor,
    disabled_foreground: ThemeColor,
) -> ButtonAppearance {
    ButtonAppearance {
        fill: Some(fill),
        foreground,
        border: None,
        border_rests_only: false,
        hover_fill: hover,
        hover_foreground: foreground,
        active_fill: active,
        selected_fill: active,
        selected_foreground: foreground,
        disabled_fill: Some(disabled),
        disabled_foreground,
    }
}

#[derive(Clone, Copy, Default)]
struct ButtonFlags(u16);

impl ButtonFlags {
    const SELECTED: u16 = 1 << 0;
    const OPEN: u16 = 1 << 1;
    const DISABLED: u16 = 1 << 2;
    const LOADING: u16 = 1 << 3;
    const ROUND: u16 = 1 << 4;
    const DANGER_ON_HOVER: u16 = 1 << 5;
    const TRIGGER: u16 = 1 << 6;
    const FULL_WIDTH: u16 = 1 << 7;
    const STRONG: u16 = 1 << 8;
    const SKIP_TAB_STOP: u16 = 1 << 9;
    const TEXT_TOGGLE: u16 = 1 << 10;

    const fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    fn set(&mut self, flag: u16, enabled: bool) {
        if enabled {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }
}

/// The fill, text and outline a button paints for its current state.
#[derive(Clone, Copy)]
struct ResolvedStyle {
    fill: Option<ThemeColor>,
    foreground: ThemeColor,
    /// The visible outline; `None` keeps the border transparent.
    border: Option<ThemeColor>,
}

impl ResolvedStyle {
    fn new(appearance: &ButtonAppearance, flags: ButtonFlags) -> Self {
        let disabled = flags.has(ButtonFlags::DISABLED);
        let engaged = flags.has(ButtonFlags::SELECTED) || flags.has(ButtonFlags::OPEN);
        let (fill, foreground) = if disabled {
            (
                appearance.disabled_fill.or(appearance.fill),
                appearance.disabled_foreground,
            )
        } else if engaged && flags.has(ButtonFlags::TEXT_TOGGLE) {
            // A text toggle marks its on state with the text alone; the surface stays put.
            (appearance.fill, appearance.selected_foreground)
        } else if engaged {
            (
                Some(appearance.selected_fill),
                appearance.selected_foreground,
            )
        } else {
            (appearance.fill, appearance.foreground)
        };
        // A disabled outline keeps its outline: disabling changes only the foreground.
        let border = appearance
            .border
            .filter(|_| !(appearance.border_rests_only && engaged && !disabled));
        Self {
            fill,
            foreground,
            border,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ButtonPolicy {
    disabled: bool,
    loading: bool,
    has_activation: bool,
}

impl ButtonPolicy {
    const fn accepts_input(self) -> bool {
        !self.disabled && !self.loading && self.has_activation
    }
}

enum Activation {
    Click(ClickActivation),
    Press(PressActivation),
}

/// The desktop's push/toggle button. See the module docs for the contract.
#[derive(IntoElement)]
pub(crate) struct Button {
    id: ElementId,
    base: BaseButton,
    theme: AerisTheme,
    variant: ButtonVariant,
    size: ButtonSize,
    layout: StyleRefinement,
    icon: Option<Icon>,
    leading: Option<AnyElement>,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    trailing: Option<AnyElement>,
    caret: Option<Icon>,
    loading_icon: Option<Icon>,
    tooltip: Option<TooltipSpec>,
    activation: Option<Activation>,
    aria_label: Option<SharedString>,
    flags: ButtonFlags,
}

impl Button {
    /// A `Default`-variant, `Default`-size button, as in the Theme System.
    pub(crate) fn new(id: impl Into<ElementId>, theme: &AerisTheme) -> Self {
        let id = id.into();
        Self {
            base: BaseButton::new(id.clone()),
            id,
            theme: *theme,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            layout: StyleRefinement::default(),
            icon: None,
            leading: None,
            label: None,
            children: Vec::new(),
            trailing: None,
            caret: None,
            loading_icon: None,
            tooltip: None,
            activation: None,
            aria_label: None,
            flags: ButtonFlags::default(),
        }
    }

    pub(crate) fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub(crate) fn button_size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// A full-radius pill or circle instead of the size's radius.
    pub(crate) fn round(mut self) -> Self {
        self.flags.set(ButtonFlags::ROUND, true);
        self
    }

    /// Close and delete actions turn `danger` on hover.
    pub(crate) fn danger_on_hover(mut self) -> Self {
        self.flags.set(ButtonFlags::DANGER_ON_HOVER, true);
        self
    }

    /// Select-trigger layout: the label and leading content on the left, the trailing slot and
    /// caret on the right.
    pub(crate) fn trigger(mut self) -> Self {
        self.flags.set(ButtonFlags::TRIGGER, true);
        self
    }

    pub(crate) fn full_width(mut self) -> Self {
        self.flags.set(ButtonFlags::FULL_WIDTH, true);
        self
    }

    /// The Theme System `Strong` label weight for primary trade actions.
    pub(crate) fn strong(mut self) -> Self {
        self.flags.set(ButtonFlags::STRONG, true);
        self
    }

    /// The leading glyph, painted at the size's glyph size unless the icon carries its own
    /// optical size (drawing-tool art drawn on differently padded artboards).
    pub(crate) fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(crate) fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Content after the label, such as a badge; pushed to the far edge by [`Button::trigger`].
    pub(crate) fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub(crate) fn caret(mut self, caret: Icon) -> Self {
        self.caret = Some(caret);
        self
    }

    pub(crate) fn loading_icon(mut self, icon: Icon) -> Self {
        self.loading_icon = Some(icon);
        self
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.flags.set(ButtonFlags::SELECTED, selected);
        self
    }

    /// A toggle that shows its state through the interactive text tokens alone:
    /// `text-interactive` off, `text-hover` on hover and `text-active` on, with no selected
    /// fill. The chart header's panel toggles use it.
    pub(crate) fn text_toggle(mut self) -> Self {
        self.flags.set(ButtonFlags::TEXT_TOGGLE, true);
        self
    }

    /// A trigger whose menu is open: drawn like the pressed state until the menu closes.
    pub(crate) fn open(mut self, open: bool) -> Self {
        self.flags.set(ButtonFlags::OPEN, open);
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.flags.set(ButtonFlags::DISABLED, disabled);
        self
    }

    pub(crate) fn loading(mut self, loading: bool) -> Self {
        self.flags.set(ButtonFlags::LOADING, loading);
        self
    }

    pub(crate) fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    pub(crate) fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.flags.set(ButtonFlags::SKIP_TAB_STOP, !tab_stop);
        self
    }

    pub(crate) fn tooltip(mut self, tooltip: TooltipSpec) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Activates on release, the default for buttons outside overlays.
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Activation::Click(Rc::new(handler)));
        self
    }

    /// Activates on the primary-button press, or on a keyboard click, and hands the handler the
    /// pointer position. Controls inside overlays use this so the scrim that dismisses on
    /// mouse-down never sees their press.
    pub(crate) fn on_press(
        mut self,
        handler: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Activation::Press(Rc::new(handler)));
        self
    }

    fn policy(&self) -> ButtonPolicy {
        ButtonPolicy {
            disabled: self.flags.has(ButtonFlags::DISABLED),
            loading: self.flags.has(ButtonFlags::LOADING),
            has_activation: self.activation.is_some(),
        }
    }

    fn appearance(&self) -> ButtonAppearance {
        let appearance = button_appearance(
            &self.theme,
            self.variant,
            self.flags.has(ButtonFlags::DANGER_ON_HOVER),
        );
        if self.flags.has(ButtonFlags::TEXT_TOGGLE) {
            text_toggle_appearance(appearance, &self.theme)
        } else {
            appearance
        }
    }

    fn radius(&self) -> Pixels {
        let token = if self.flags.has(ButtonFlags::ROUND) {
            RadiusToken::Full
        } else {
            self.size.radius()
        };
        px(f32::from(token.logical_pixels()))
    }

    /// Only a lone glyph makes a square button; any text, trailing content or caret pads it.
    fn is_square(&self) -> bool {
        self.label.is_none()
            && self.children.is_empty()
            && self.trailing.is_none()
            && self.caret.is_none()
            && !self.flags.has(ButtonFlags::TRIGGER)
    }

    fn leading_icon(&mut self, icon_size: Pixels) -> Option<AnyElement> {
        if self.flags.has(ButtonFlags::LOADING) {
            let loader_id = ElementId::NamedChild(Arc::new(self.id.clone()), "loader".into());
            self.loading_icon
                .take()
                .or_else(|| self.icon.take())
                .map(|icon| {
                    Loader::new(loader_id, icon)
                        .with_size(icon_size)
                        .into_any_element()
                })
        } else {
            self.icon
                .take()
                .map(|icon| icon.or_size(icon_size).into_any_element())
        }
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.layout
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

/// What a button shows, in paint order.
struct ButtonContent {
    leading_icon: Option<AnyElement>,
    leading: Option<AnyElement>,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    trailing: Option<AnyElement>,
    caret: Option<AnyElement>,
}

impl ButtonContent {
    fn attach(self, button: BaseButton, trigger: bool) -> BaseButton {
        if !trigger {
            return button
                .children(self.leading_icon)
                .children(self.leading)
                .children(self.label)
                .children(self.children)
                .children(self.trailing)
                .children(self.caret);
        }
        button
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .children(self.leading_icon)
                    .children(self.leading)
                    .children(
                        self.label
                            .map(|label| div().min_w_0().truncate().child(label)),
                    )
                    .children(self.children),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .children(self.trailing)
                    .children(self.caret),
            )
    }
}

/// Geometry, typography and state colours of the button frame.
fn style_frame(
    button: BaseButton,
    size: ButtonSize,
    flags: ButtonFlags,
    square: bool,
    radius: Pixels,
) -> BaseButton {
    let label_role = if flags.has(ButtonFlags::STRONG) {
        TypographyRole::Strong
    } else {
        TypographyRole::Normal
    };
    button
        .flex()
        .flex_shrink_0()
        .relative()
        .items_center()
        .gap_1()
        .h(design_rems(size.logical_height()))
        .rounded(radius)
        .font_family(platform_font_family())
        .font_weight(platform_font_weight(label_role))
        .map(|button| {
            if size.dense_text() {
                button.text_xs()
            } else {
                button.text_sm()
            }
        })
        .map(|button| {
            if square {
                button.w(design_rems(size.logical_height()))
            } else {
                button.px(design_rems(size.logical_padding_x()))
            }
        })
        .map(|button| {
            if flags.has(ButtonFlags::TRIGGER) {
                button.justify_between()
            } else {
                button.justify_center()
            }
        })
        .when(flags.has(ButtonFlags::FULL_WIDTH), Styled::w_full)
        .when(flags.has(ButtonFlags::DISABLED), Styled::cursor_not_allowed)
        .when(flags.has(ButtonFlags::LOADING), |button| {
            button.cursor_default().opacity(0.8)
        })
}

fn paint_states(
    button: BaseButton,
    appearance: ButtonAppearance,
    resolved: ResolvedStyle,
    border_width: Pixels,
    focus_ring: ThemeColor,
    accepts_input: bool,
    engaged: bool,
) -> BaseButton {
    let drop_border = appearance.border_rests_only;
    // Every variant keeps a border of `--border-width`, transparent unless it is outlined, so a
    // state change never shifts the layout.
    button
        .when_some(resolved.fill, |button, fill| button.bg(gpui_color(fill)))
        .text_color(gpui_color(resolved.foreground))
        .border(border_width)
        .border_color(
            resolved
                .border
                .map_or_else(gpui::transparent_black, gpui_color),
        )
        .when(accepts_input, |button| {
            button
                .cursor_pointer()
                .hover(move |style| {
                    if engaged {
                        return style;
                    }
                    let style = style
                        .bg(gpui_color(appearance.hover_fill))
                        .text_color(gpui_color(appearance.hover_foreground));
                    if drop_border {
                        style.border_color(gpui::transparent_black())
                    } else {
                        style
                    }
                })
                .active(move |style| {
                    let style = style.bg(gpui_color(appearance.active_fill));
                    if drop_border {
                        style.border_color(gpui::transparent_black())
                    } else {
                        style
                    }
                })
        })
        // The Theme System focus style for every variant: a 2px `ring-primary` outline.
        .focus_visible(move |style| style.border_2().border_color(gpui_color(focus_ring)))
}

/// Registers the button's single activation path.
fn attach_activation(button: BaseButton, activation: Option<Activation>) -> BaseButton {
    match activation {
        Some(Activation::Click(handler)) => button.on_click(move |event, window, cx| {
            handler(event, window, cx);
            cx.stop_propagation();
        }),
        Some(Activation::Press(handler)) => {
            let keyboard = handler.clone();
            button
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    handler(event.position, window, cx);
                    cx.stop_propagation();
                })
                // The press already acted; the release that follows belongs to this button too,
                // so a clickable row underneath never activates with it.
                .on_click(move |event, window, cx| {
                    if matches!(event, ClickEvent::Keyboard(_)) {
                        keyboard(event.position(), window, cx);
                    }
                    cx.stop_propagation();
                })
        }
        None => button,
    }
}

impl RenderOnce for Button {
    fn render(mut self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let policy = self.policy();
        let appearance = self.appearance();
        let resolved = ResolvedStyle::new(&appearance, self.flags);
        let flags = self.flags;
        let square = self.is_square();
        let radius = self.radius();
        let border_width = platform_border_width(&self.theme);
        let icon_size = self.size.icon();
        let caret_size = (icon_size * 0.75).max(px(10.0));
        let content = ButtonContent {
            leading_icon: self.leading_icon(icon_size),
            leading: self.leading,
            label: self.label.clone(),
            children: self.children,
            trailing: self.trailing,
            caret: self
                .caret
                .map(|caret| caret.with_size(caret_size).into_any_element()),
        };
        let aria_label = self.aria_label.or(self.label);

        let button = self
            .base
            .occlude()
            .when_some(aria_label, BaseButton::accessibility_label)
            .selected(flags.has(ButtonFlags::SELECTED))
            .disabled(flags.has(ButtonFlags::DISABLED))
            .tab_stop(!flags.has(ButtonFlags::SKIP_TAB_STOP) && !flags.has(ButtonFlags::DISABLED))
            .aria_selected(flags.has(ButtonFlags::SELECTED));
        let button = style_frame(button, self.size, flags, square, radius);
        let button = paint_states(
            button,
            appearance,
            resolved,
            border_width,
            self.theme.colors.ring_primary,
            policy.accepts_input(),
            flags.has(ButtonFlags::SELECTED) || flags.has(ButtonFlags::OPEN),
        );
        let button = attach_activation(button, self.activation.filter(|_| policy.accepts_input()));
        let mut button = content.attach(button, flags.has(ButtonFlags::TRIGGER));
        button.style().refine(&self.layout);

        match self.tooltip {
            Some(tooltip) => {
                let delay = tooltip.delay();
                button
                    .tooltip(tooltip.builder())
                    .tooltip_show_delay(delay)
                    .into_any_element()
            }
            None => button.into_any_element(),
        }
    }
}

/// The one close control for dialogs, panels and menus: a small round ghost button that turns
/// `danger` on hover. It activates on press so the overlay it closes never sees the mouse-down.
pub(crate) fn close_button(
    id: impl Into<ElementId>,
    theme: &AerisTheme,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id, theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .round()
        .danger_on_hover()
        .icon(Icon::new(UiIcon::Close.path()))
        .aria_label("Close")
        .on_press(move |_, window, cx| on_close(window, cx))
}

#[cfg(test)]
mod tests {
    use aeris_design_system::AerisTheme;
    use gpui::px;

    use aeris_design_system::RadiusToken;

    use super::{
        ButtonFlags, ButtonPolicy, ButtonSize, ButtonVariant, ResolvedStyle, button_appearance,
        text_toggle_appearance,
    };

    #[test]
    fn text_toggles_mark_their_state_with_text_only() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let c = theme.colors;
            let toggle = text_toggle_appearance(
                button_appearance(&theme, ButtonVariant::Ghost, false),
                &theme,
            );
            let mut flags = ButtonFlags::default();
            flags.set(ButtonFlags::TEXT_TOGGLE, true);
            let off = ResolvedStyle::new(&toggle, flags);
            assert_eq!(off.fill, None);
            assert_eq!(off.foreground, c.text_interactive);
            assert_eq!(toggle.hover_foreground, c.text_hover);
            flags.set(ButtonFlags::SELECTED, true);
            let on = ResolvedStyle::new(&toggle, flags);
            assert_eq!(on.fill, None, "the on state adds no background");
            assert_eq!(on.foreground, c.text_active);
        }
    }

    #[test]
    fn sizes_follow_the_theme_system_button_sizes() {
        let geometry: Vec<(f32, f32, RadiusToken)> =
            [ButtonSize::Sm, ButtonSize::Default, ButtonSize::Lg]
                .into_iter()
                .map(|size| {
                    (
                        size.logical_height(),
                        size.logical_padding_x(),
                        size.radius(),
                    )
                })
                .collect();
        // sm: h-6 px-2, default: h-7 px-2.5, lg: h-8 px-3 rounded-default.
        assert_eq!(
            geometry,
            [
                (24.0, 8.0, RadiusToken::Compact),
                (28.0, 10.0, RadiusToken::Compact),
                (32.0, 12.0, RadiusToken::Default),
            ]
        );
        assert_eq!(ButtonSize::default(), ButtonSize::Default);
        assert_eq!(ButtonVariant::default(), ButtonVariant::Default);
    }

    #[test]
    fn glyphs_grow_with_the_button_size() {
        assert_eq!(ButtonSize::Lg.icon(), px(18.0));
        assert_eq!(ButtonSize::Default.icon(), px(16.0));
        assert_eq!(ButtonSize::Sm.icon(), px(16.0));
    }

    #[test]
    fn small_icon_buttons_centre_their_glyph_on_whole_device_pixels() {
        // GPUI snaps the hover fill and the glyph to device pixels independently, so the inset
        // must be a whole, equal number of device pixels on both sides at every supported scale.
        // Round panel actions and close buttons are `Sm`; 24/16 holds at 100–200 %.
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
            let hit = ButtonSize::Sm.logical_height() * scale;
            let glyph = f32::from(ButtonSize::Sm.icon()) * scale;
            let inset = (hit - glyph) / 2.0;
            for (name, device_pixels) in [("hit", hit), ("glyph", glyph), ("inset", inset)] {
                assert!(
                    device_pixels.fract().abs() < f32::EPSILON,
                    "{name} is {device_pixels} device px at {scale}x"
                );
            }
        }
    }

    #[test]
    fn activation_requires_one_enabled_handler() {
        let enabled = ButtonPolicy {
            disabled: false,
            loading: false,
            has_activation: true,
        };
        assert!(enabled.accepts_input());
        for policy in [
            ButtonPolicy {
                disabled: true,
                ..enabled
            },
            ButtonPolicy {
                loading: true,
                ..enabled
            },
            ButtonPolicy {
                has_activation: false,
                ..enabled
            },
        ] {
            assert!(!policy.accepts_input());
        }
    }

    /// Each filled variant is one Theme System token ramp: `(fill, text, hover, press, disabled
    /// fill, disabled text)`.
    #[test]
    fn filled_variants_match_the_theme_system_button() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let c = theme.colors;
            for (variant, ramp) in [
                (
                    ButtonVariant::Default,
                    (
                        c.button_fill,
                        c.button_fill_foreground,
                        c.button_fill_hover,
                        c.button_fill_active,
                        c.disabled_bg,
                        c.text_muted,
                    ),
                ),
                (
                    ButtonVariant::Secondary,
                    (
                        c.primary,
                        c.primary_foreground,
                        c.primary_hover,
                        c.primary_active,
                        c.primary_disabled,
                        c.primary_disabled_foreground,
                    ),
                ),
                (
                    ButtonVariant::Destructive,
                    (
                        c.danger,
                        c.danger_foreground,
                        c.danger_hover,
                        c.danger_active,
                        c.danger_disabled,
                        c.danger_disabled_foreground,
                    ),
                ),
                (
                    ButtonVariant::Buy,
                    (
                        c.buy,
                        c.buy_foreground,
                        c.buy_hover,
                        c.buy_active,
                        c.buy_disabled,
                        c.buy_disabled_foreground,
                    ),
                ),
                (
                    ButtonVariant::Sell,
                    (
                        c.sell,
                        c.sell_foreground,
                        c.sell_hover,
                        c.sell_active,
                        c.sell_disabled,
                        c.sell_disabled_foreground,
                    ),
                ),
            ] {
                let appearance = button_appearance(&theme, variant, false);
                assert_eq!(
                    (
                        appearance.fill,
                        appearance.foreground,
                        appearance.hover_fill,
                        appearance.active_fill,
                        appearance.disabled_fill,
                        appearance.disabled_foreground,
                    ),
                    (Some(ramp.0), ramp.1, ramp.2, ramp.3, Some(ramp.4), ramp.5),
                    "{variant:?}"
                );
                assert_eq!(appearance.border, None, "{variant:?} has no outline");
            }
        }
    }

    #[test]
    fn outline_rests_on_surface_and_drops_its_border_when_engaged() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let c = theme.colors;
            let outline = button_appearance(&theme, ButtonVariant::Outline, false);
            // border-border bg-surface text-text-default
            assert_eq!(outline.fill, Some(c.surface));
            assert_eq!(outline.border, Some(c.border));
            assert_eq!(outline.foreground, c.text_default);
            // hover:border-transparent hover:bg-hover-bg, active:bg-active-bg
            assert!(outline.border_rests_only);
            assert_eq!(outline.hover_fill, c.hover_bg);
            assert_eq!(outline.active_fill, c.active_bg);
            // Disabled keeps the resting surface and outline; only the foreground mutes.
            let mut disabled = ButtonFlags::default();
            disabled.set(ButtonFlags::DISABLED, true);
            let resolved = ResolvedStyle::new(&outline, disabled);
            assert_eq!(resolved.border, Some(c.border));
            assert_eq!(resolved.fill, Some(c.surface));
            assert_eq!(resolved.foreground, c.text_muted);
            let resting = ResolvedStyle::new(&outline, ButtonFlags::default());
            assert_eq!(resting.border, Some(c.border));
        }
    }

    #[test]
    fn ghost_is_text_only_until_hovered() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let c = theme.colors;
            let ghost = button_appearance(&theme, ButtonVariant::Ghost, false);
            // border-transparent text-text-default hover:bg-hover-bg active:bg-active-bg
            assert_eq!(ghost.fill, None);
            assert_eq!(ghost.border, None);
            assert_eq!(ghost.foreground, c.text_default);
            assert_eq!(ghost.hover_fill, c.hover_bg);
            assert_eq!(ghost.active_fill, c.active_bg);
            // A disabled icon action stays transparent; only its glyph mutes.
            assert_eq!(ghost.disabled_fill, None);
            assert_eq!(ghost.disabled_foreground, c.text_muted);
            let mut disabled = ButtonFlags::default();
            disabled.set(ButtonFlags::DISABLED, true);
            let resolved = ResolvedStyle::new(&ghost, disabled);
            assert_eq!(resolved.fill, None);
            assert_eq!(resolved.foreground, c.text_muted);
            // A ghost toggle that is on rests on the pressed fill.
            assert_eq!(ghost.selected_fill, c.active_bg);
            assert_eq!(ghost.selected_foreground, c.text_active);
        }
    }

    #[test]
    fn close_actions_turn_danger_on_hover() {
        let theme = AerisTheme::light();
        let colors = theme.colors;
        let close = button_appearance(&theme, ButtonVariant::Ghost, true);
        assert_eq!(close.fill, None);
        assert_eq!(close.hover_fill, colors.danger);
        assert_eq!(close.hover_foreground, colors.danger_foreground);
        assert_eq!(close.active_fill, colors.danger_active);
    }
}
