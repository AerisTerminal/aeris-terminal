//! `Aeris`'s one push/toggle button.
//!
//! Every clickable control in the desktop is a [`Button`]: a [`ButtonVariant`] picks its colours,
//! a [`ButtonSize`] picks its geometry and text size, and the state builders (`selected`, `open`,
//! `disabled`, `loading`) restyle it from tokens. Callers add only layout (`flex_1`, margins, an
//! explicit width, or a height for a strip such as a disclosure arrow); they never restyle a
//! button's colours, borders, radius or hover states.
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

/// What a button looks like at rest, hovered, pressed, selected and disabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ButtonVariant {
    /// The neutral default call to action (`button-fill`).
    Filled,
    /// A raised neutral action on `surface-secondary` with a `border-secondary` outline.
    Secondary,
    /// A bordered action on its resting surface: steppers, filters and select triggers.
    Outline,
    /// Borderless chrome action that takes the surface it sits on and uses the interactive
    /// text and icon tokens. Toolbar, header and panel icon actions are ghost buttons.
    Ghost,
    /// Irreversible action (`danger-*`).
    Destructive,
    /// Affirmative confirm and buy action. The `positive` status token has no interaction
    /// states, so this uses the `buy-*` ramp, which carries the same hue with every state.
    Positive,
    /// Sell action (`sell-*`), the trade counterpart of [`ButtonVariant::Positive`].
    Negative,
}

/// The one height scale every button uses. Icon-only buttons are square at this height.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ButtonSize {
    /// 22 px: dense panel-header filters and triggers.
    Xs,
    /// 24 px: round panel actions and chart-header controls.
    Sm,
    /// 28 px: the Theme System default (`h-7`) for dialog and form actions.
    #[default]
    Md,
    /// 32 px: toolbars and order-ticket actions.
    Lg,
    /// 34 px: full-width select triggers in forms.
    Xl,
}

/// Geometry is in design rems so a button grows with an enclosing [`RemScale`] panel and renders
/// at its exact logical size everywhere else; glyphs stay pixel-aligned.
///
/// [`RemScale`]: super::rem_scale::RemScale
impl ButtonSize {
    /// Logical height in pixels at the root rem size.
    pub(crate) const fn logical_height(self) -> f32 {
        match self {
            Self::Xs => 22.0,
            Self::Sm => 24.0,
            Self::Md => 28.0,
            Self::Lg => 32.0,
            Self::Xl => 34.0,
        }
    }

    const fn logical_padding_x(self) -> f32 {
        match self {
            Self::Xs => 6.0,
            Self::Sm => 8.0,
            Self::Md => 10.0,
            Self::Lg | Self::Xl => 12.0,
        }
    }

    /// Glyph size for an icon that does not carry its own optical size.
    fn icon(self) -> Pixels {
        px(match self {
            Self::Xs => 14.0,
            Self::Sm | Self::Md => 16.0,
            Self::Lg | Self::Xl => 18.0,
        })
    }

    const fn dense_text(self) -> bool {
        matches!(self, Self::Xs | Self::Sm)
    }
}

/// Resolved token colours for one variant on one resting surface.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ButtonAppearance {
    /// `None` leaves the button transparent over whatever it sits on.
    fill: Option<ThemeColor>,
    foreground: ThemeColor,
    border: Option<ThemeColor>,
    hover_fill: ThemeColor,
    hover_foreground: ThemeColor,
    active_fill: ThemeColor,
    selected_fill: ThemeColor,
    selected_foreground: ThemeColor,
    open_border: Option<ThemeColor>,
    disabled_fill: Option<ThemeColor>,
    disabled_foreground: ThemeColor,
    focus_ring: ThemeColor,
}

fn button_appearance(
    theme: &AerisTheme,
    variant: ButtonVariant,
    resting: Option<ThemeColor>,
    danger_on_hover: bool,
) -> ButtonAppearance {
    let colors = theme.colors;
    let mut appearance = match variant {
        ButtonVariant::Filled => ButtonAppearance {
            fill: Some(colors.button_fill),
            foreground: colors.button_fill_foreground,
            border: None,
            hover_fill: colors.button_fill_hover,
            hover_foreground: colors.button_fill_foreground,
            active_fill: colors.button_fill_active,
            selected_fill: colors.button_fill_active,
            selected_foreground: colors.button_fill_foreground,
            open_border: None,
            disabled_fill: Some(colors.disabled_bg),
            disabled_foreground: colors.text_muted,
            focus_ring: colors.ring,
        },
        ButtonVariant::Secondary | ButtonVariant::Outline | ButtonVariant::Ghost => {
            let (default_resting, foreground, border, selected_foreground) = match variant {
                ButtonVariant::Secondary => (
                    Some(colors.surface_secondary),
                    colors.text_primary,
                    Some(colors.border_secondary),
                    colors.text_primary,
                ),
                ButtonVariant::Outline => (
                    Some(colors.surface),
                    colors.text_primary,
                    Some(colors.border),
                    colors.text_primary,
                ),
                _ => (None, colors.text_interactive, None, colors.text_active),
            };
            let fill = resting.or(default_resting);
            // State tokens composite over an opaque resting surface, and paint as they are on a
            // transparent ghost button.
            let state = |token: ThemeColor| fill.map_or(token, |fill| token.over(fill));
            let hover_foreground = if border.is_some() {
                foreground
            } else {
                colors.text_hover
            };
            ButtonAppearance {
                fill,
                foreground,
                border,
                hover_fill: state(colors.hover_bg),
                hover_foreground,
                active_fill: state(colors.active_bg),
                selected_fill: state(colors.active_bg),
                selected_foreground,
                open_border: border.map(|_| colors.border_strong),
                disabled_fill: fill,
                disabled_foreground: colors.text_muted,
                focus_ring: colors.ring,
            }
        }
        ButtonVariant::Destructive => ramp(
            colors.danger,
            colors.danger_foreground,
            colors.danger_hover,
            colors.danger_active,
            colors.danger_disabled,
            colors.danger_disabled_foreground,
            colors.danger_ring,
        ),
        ButtonVariant::Positive => ramp(
            colors.buy,
            colors.buy_foreground,
            colors.buy_hover,
            colors.buy_active,
            colors.buy_disabled,
            colors.buy_disabled_foreground,
            colors.buy_ring,
        ),
        ButtonVariant::Negative => ramp(
            colors.sell,
            colors.sell_foreground,
            colors.sell_hover,
            colors.sell_active,
            colors.sell_disabled,
            colors.sell_disabled_foreground,
            colors.sell_ring,
        ),
    };
    if danger_on_hover {
        appearance.hover_fill = colors.danger;
        appearance.hover_foreground = colors.danger_foreground;
        appearance.active_fill = colors.danger_active;
    }
    appearance
}

const fn ramp(
    fill: ThemeColor,
    foreground: ThemeColor,
    hover: ThemeColor,
    active: ThemeColor,
    disabled: ThemeColor,
    disabled_foreground: ThemeColor,
    ring: ThemeColor,
) -> ButtonAppearance {
    ButtonAppearance {
        fill: Some(fill),
        foreground,
        border: None,
        hover_fill: hover,
        hover_foreground: foreground,
        active_fill: active,
        selected_fill: active,
        selected_foreground: foreground,
        open_border: None,
        disabled_fill: Some(disabled),
        disabled_foreground,
        focus_ring: ring,
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
    border: Option<ThemeColor>,
}

impl ResolvedStyle {
    fn new(appearance: &ButtonAppearance, flags: ButtonFlags) -> Self {
        let (fill, foreground) = if flags.has(ButtonFlags::DISABLED) {
            (appearance.disabled_fill, appearance.disabled_foreground)
        } else if flags.has(ButtonFlags::SELECTED) || flags.has(ButtonFlags::OPEN) {
            (
                Some(appearance.selected_fill),
                appearance.selected_foreground,
            )
        } else {
            (appearance.fill, appearance.foreground)
        };
        let border = if flags.has(ButtonFlags::OPEN) {
            appearance.open_border.or(appearance.border)
        } else {
            appearance.border
        };
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
    resting_fill: Option<ThemeColor>,
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
    /// A medium ghost button.
    pub(crate) fn new(id: impl Into<ElementId>, theme: &AerisTheme) -> Self {
        let id = id.into();
        Self {
            base: BaseButton::new(id.clone()),
            id,
            theme: *theme,
            variant: ButtonVariant::Ghost,
            size: ButtonSize::default(),
            resting_fill: None,
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

    /// The opaque surface a ghost, outline or secondary button rests on. Hover, press and
    /// selected fills composite the alpha tokens over it instead of replacing it.
    pub(crate) fn resting_fill(mut self, fill: ThemeColor) -> Self {
        self.resting_fill = Some(fill);
        self
    }

    /// A full-radius pill or circle instead of `--radius-button`.
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

    /// A trigger whose menu is open: the selected fill plus the `border-strong` outline.
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
        button_appearance(
            &self.theme,
            self.variant,
            self.resting_fill,
            self.flags.has(ButtonFlags::DANGER_ON_HOVER),
        )
    }

    fn radius(&self) -> Pixels {
        let token = if self.flags.has(ButtonFlags::ROUND) {
            RadiusToken::Full
        } else {
            RadiusToken::Button
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
    accepts_input: bool,
) -> BaseButton {
    button
        .when_some(resolved.fill, |button, fill| button.bg(gpui_color(fill)))
        .text_color(gpui_color(resolved.foreground))
        .when_some(resolved.border, |button, border| {
            button.border(border_width).border_color(gpui_color(border))
        })
        .when(accepts_input, |button| {
            button
                .cursor_pointer()
                .hover(move |style| {
                    style
                        .bg(gpui_color(appearance.hover_fill))
                        .text_color(gpui_color(appearance.hover_foreground))
                })
                .active(move |style| style.bg(gpui_color(appearance.active_fill)))
        })
        .focus_visible(move |style| {
            style
                .border_2()
                .border_color(gpui_color(appearance.focus_ring))
        })
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
            policy.accepts_input(),
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

    use super::{ButtonPolicy, ButtonSize, ButtonVariant, button_appearance};

    #[test]
    fn sizes_follow_the_one_height_scale() {
        let heights: Vec<f32> = [
            ButtonSize::Xs,
            ButtonSize::Sm,
            ButtonSize::Md,
            ButtonSize::Lg,
            ButtonSize::Xl,
        ]
        .into_iter()
        .map(ButtonSize::logical_height)
        .collect();
        assert_eq!(heights, [22.0, 24.0, 28.0, 32.0, 34.0]);
        assert_eq!(ButtonSize::default(), ButtonSize::Md);
    }

    #[test]
    fn glyphs_grow_with_the_button_size() {
        assert_eq!(ButtonSize::Xl.icon(), px(18.0));
        assert_eq!(ButtonSize::Lg.icon(), px(18.0));
        assert_eq!(ButtonSize::Md.icon(), px(16.0));
        assert_eq!(ButtonSize::Sm.icon(), px(16.0));
        assert_eq!(ButtonSize::Xs.icon(), px(14.0));
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

    #[test]
    fn variants_use_their_token_ramps_in_both_modes() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let colors = theme.colors;
            let filled = button_appearance(&theme, ButtonVariant::Filled, None, false);
            assert_eq!(filled.fill, Some(colors.button_fill));
            assert_eq!(filled.foreground, colors.button_fill_foreground);
            assert_eq!(filled.hover_fill, colors.button_fill_hover);

            let secondary = button_appearance(&theme, ButtonVariant::Secondary, None, false);
            assert_eq!(secondary.fill, Some(colors.surface_secondary));
            assert_eq!(secondary.border, Some(colors.border_secondary));

            let outline = button_appearance(&theme, ButtonVariant::Outline, None, false);
            assert_eq!(outline.fill, Some(colors.surface));
            assert_eq!(outline.border, Some(colors.border));
            assert_eq!(outline.open_border, Some(colors.border_strong));

            let destructive = button_appearance(&theme, ButtonVariant::Destructive, None, false);
            assert_eq!(destructive.fill, Some(colors.danger));
            assert_eq!(destructive.disabled_fill, Some(colors.danger_disabled));
            assert_eq!(destructive.focus_ring, colors.danger_ring);

            let positive = button_appearance(&theme, ButtonVariant::Positive, None, false);
            assert_eq!(positive.fill, Some(colors.buy));
            assert_eq!(positive.hover_fill, colors.buy_hover);

            let negative = button_appearance(&theme, ButtonVariant::Negative, None, false);
            assert_eq!(negative.fill, Some(colors.sell));
            assert_eq!(negative.active_fill, colors.sell_active);
            assert_eq!(
                negative.disabled_foreground,
                colors.sell_disabled_foreground
            );
        }
    }

    #[test]
    fn ghost_buttons_composite_states_over_their_resting_surface() {
        let theme = AerisTheme::dark();
        let colors = theme.colors;
        let ghost = button_appearance(
            &theme,
            ButtonVariant::Ghost,
            Some(colors.surface_secondary),
            false,
        );
        assert_eq!(ghost.fill, Some(colors.surface_secondary));
        assert_eq!(ghost.foreground, colors.text_interactive);
        assert_eq!(
            ghost.hover_fill,
            colors.hover_bg.over(colors.surface_secondary)
        );
        assert_eq!(ghost.hover_foreground, colors.text_hover);
        assert_eq!(
            ghost.selected_fill,
            colors.active_bg.over(colors.surface_secondary)
        );
        assert_eq!(ghost.selected_foreground, colors.text_active);
        assert_eq!(ghost.disabled_foreground, colors.text_muted);
        assert_eq!(ghost.border, None);
    }

    #[test]
    fn ghost_buttons_without_a_surface_stay_transparent_and_use_raw_state_tokens() {
        let theme = AerisTheme::light();
        let colors = theme.colors;
        let ghost = button_appearance(&theme, ButtonVariant::Ghost, None, false);
        assert_eq!(ghost.fill, None);
        assert_eq!(ghost.disabled_fill, None);
        assert_eq!(ghost.hover_fill, colors.hover_bg);
        assert_eq!(ghost.selected_fill, colors.active_bg);
    }

    #[test]
    fn close_actions_turn_danger_on_hover() {
        let theme = AerisTheme::light();
        let colors = theme.colors;
        let close = button_appearance(&theme, ButtonVariant::Ghost, None, true);
        assert_eq!(close.fill, None);
        assert_eq!(close.hover_fill, colors.danger);
        assert_eq!(close.hover_foreground, colors.danger_foreground);
        assert_eq!(close.active_fill, colors.danger_active);
    }
}
