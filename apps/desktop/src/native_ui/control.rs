use std::{rc::Rc, sync::Arc};

use asceify_design_system::{
    AsceifyTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement, Interactivity, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, StyleRefinement, Styled, Window, div,
    prelude::*, px,
};
use gpui_base::Button as BaseButton;

use super::{
    icon::Icon,
    loader::Loader,
    platform_font_weight,
    theme::{
        ButtonAppearance, ButtonVariant, button_appearance, gpui_color, platform_border_width,
    },
    tooltip::TooltipSpec,
};

type Activation = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

const DEFAULT_CONTROL_SIZE: Pixels = px(32.0);
const DEFAULT_ICON_SIZE: Pixels = px(16.0);
const CUSTOM_ICON_SCALE: f32 = 0.75;

fn control_geometry(content_size: Option<Pixels>) -> (Pixels, Pixels) {
    content_size.map_or((DEFAULT_CONTROL_SIZE, DEFAULT_ICON_SIZE), |control_size| {
        (control_size, control_size * CUSTOM_ICON_SCALE)
    })
}

fn control_label_weight() -> gpui::FontWeight {
    platform_font_weight(TypographyRole::Normal)
}

fn with_pointer_states(
    control: BaseButton,
    policy: ControlPolicy,
    caller_hover_style: Option<StyleRefinement>,
    hover_color: Option<Hsla>,
    active_color: Option<Hsla>,
) -> BaseButton {
    let has_caller_hover_style = caller_hover_style.is_some();
    control
        .when_some(
            caller_hover_style.filter(|_| policy.accepts_input()),
            |this, caller_hover_style| {
                this.hover(move |mut style| {
                    style.refine(&caller_hover_style);
                    style
                })
            },
        )
        .when_some(
            hover_color.filter(|_| policy.accepts_input() && !has_caller_hover_style),
            |this, color| this.hover(move |style| style.bg(color)),
        )
        .when_some(
            active_color.filter(|_| policy.accepts_input()),
            |this, color| this.active(move |style| style.bg(color)),
        )
}

fn with_control_surface(
    control: BaseButton,
    surface: Option<ButtonAppearance>,
    theme: Option<&AsceifyTheme>,
) -> BaseButton {
    control.when_some(surface, |control, surface| {
        control
            .bg(gpui_color(surface.fill))
            .text_color(gpui_color(surface.foreground))
            .when_some(surface.border, |control, border| {
                control
                    .border(theme.map_or(px(1.0), platform_border_width))
                    .border_color(gpui_color(border))
            })
    })
}

#[derive(Clone, Copy)]
struct ControlFlags(u8);

impl ControlFlags {
    const SELECTED: u8 = 1 << 0;
    const DISABLED: u8 = 1 << 1;
    const LOADING: u8 = 1 << 2;
    const COMPACT: u8 = 1 << 3;
    const TAB_STOP: u8 = 1 << 4;

    const fn new() -> Self {
        Self(Self::TAB_STOP)
    }

    const fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    fn set(&mut self, flag: u8, enabled: bool) {
        if enabled {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ControlPolicy {
    disabled: bool,
    loading: bool,
    has_activation: bool,
}

impl ControlPolicy {
    const fn accepts_input(self) -> bool {
        !self.disabled && !self.loading && self.has_activation
    }
}

/// A narrow `Asceify`-owned push/toggle control built directly on GPUI's
/// styling primitives and `gpui-base`'s press, focus, and accessibility model.
///
/// Base owns press/release pairing and keyboard click synthesis. This control
/// deliberately registers one activation listener and no parallel mouse-down
/// or key-down activation path.
#[derive(IntoElement)]
pub(crate) struct Control {
    id: ElementId,
    base: BaseButton,
    style: StyleRefinement,
    icon: Option<Icon>,
    leading: Option<AnyElement>,
    label: Option<SharedString>,
    caret: Option<Icon>,
    loading_icon: Option<Icon>,
    children: Vec<AnyElement>,
    theme: Option<AsceifyTheme>,
    surface: Option<ButtonAppearance>,
    resting_fill: Option<ThemeColor>,
    tooltip: Option<TooltipSpec>,
    activation: Option<Activation>,
    aria_label: Option<SharedString>,
    hover_style: Option<StyleRefinement>,
    flags: ControlFlags,
    tab_index: isize,
    content_size: Option<Pixels>,
}

/// Compatibility name for the control used throughout the desktop shell.
pub(crate) type Button = Control;

impl Control {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: BaseButton::new(id.clone()),
            id,
            style: StyleRefinement::default(),
            icon: None,
            leading: None,
            label: None,
            caret: None,
            loading_icon: None,
            children: Vec::new(),
            theme: None,
            surface: None,
            resting_fill: None,
            tooltip: None,
            activation: None,
            aria_label: None,
            hover_style: None,
            flags: ControlFlags::new(),
            tab_index: 0,
            content_size: None,
        }
    }

    pub(crate) fn theme(mut self, theme: &AsceifyTheme) -> Self {
        self.theme = Some(*theme);
        self
    }

    pub(crate) fn variant(mut self, theme: &AsceifyTheme, variant: ButtonVariant) -> Self {
        self.theme = Some(*theme);
        let appearance = button_appearance(theme, variant);
        self.resting_fill = Some(appearance.fill);
        self.surface = Some(appearance);
        self.h(DEFAULT_CONTROL_SIZE)
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
    }

    /// The opaque fill this control rests on; hover/selected states composite
    /// the CSS alpha tokens over it instead of replacing it.
    pub(crate) fn resting_fill(mut self, fill: ThemeColor) -> Self {
        self.resting_fill = Some(fill);
        self
    }

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

    pub(crate) fn caret(mut self, caret: Icon) -> Self {
        self.caret = Some(caret);
        self
    }

    pub(crate) fn loading_icon(mut self, icon: Icon) -> Self {
        self.loading_icon = Some(icon);
        self
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.flags.set(ControlFlags::SELECTED, selected);
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.flags.set(ControlFlags::DISABLED, disabled);
        self
    }

    pub(crate) fn loading(mut self, loading: bool) -> Self {
        self.flags.set(ControlFlags::LOADING, loading);
        self
    }

    pub(crate) fn compact(mut self) -> Self {
        self.flags.set(ControlFlags::COMPACT, true);
        self
    }

    pub(crate) fn with_size(mut self, size: Pixels) -> Self {
        self.content_size = Some(size);
        self
    }

    pub(crate) fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    pub(crate) fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }

    pub(crate) fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.flags.set(ControlFlags::TAB_STOP, tab_stop);
        self
    }

    pub(crate) fn tooltip(mut self, tooltip: TooltipSpec) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Rc::new(handler));
        self
    }

    fn policy(&self) -> ControlPolicy {
        ControlPolicy {
            disabled: self.flags.contains(ControlFlags::DISABLED),
            loading: self.flags.contains(ControlFlags::LOADING),
            has_activation: self.activation.is_some(),
        }
    }

    fn loader_id(&self) -> ElementId {
        ElementId::NamedChild(Arc::new(self.id.clone()), "loader".into())
    }

    fn leading_element(&mut self, loader_id: ElementId, icon_size: Pixels) -> Option<AnyElement> {
        if self.flags.contains(ControlFlags::LOADING) {
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
                .map(|icon| icon.with_size(icon_size).into_any_element())
        }
    }

    fn state_colors(&self) -> (Option<Hsla>, Option<Hsla>) {
        if let Some(appearance) = self.surface {
            return (
                Some(gpui_color(appearance.hover)),
                Some(gpui_color(appearance.active)),
            );
        }
        let Some(theme) = self.theme else {
            return (None, None);
        };
        let ghost = button_appearance(&theme, ButtonVariant::Ghost);
        let backdrop = self.resting_fill.unwrap_or(ghost.fill);
        (
            Some(gpui_color(theme.colors.hover_bg.over(backdrop))),
            Some(gpui_color(theme.colors.active_bg.over(backdrop))),
        )
    }
}

impl Styled for Control {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Control {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for Control {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }

    fn hover(mut self, f: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Self {
        debug_assert!(self.hover_style.is_none(), "hover style already set");
        self.hover_style = Some(f(StyleRefinement::default()));
        self
    }
}

impl RenderOnce for Control {
    fn render(mut self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let policy = self.policy();
        let loader_id = self.loader_id();
        let focus_color = self.theme.map_or_else(
            || window.text_style().color,
            |theme| gpui_color(theme.colors.ring),
        );
        let (hover_color, selected_color) = self.state_colors();
        let disabled_color = self.theme.map(|theme| gpui_color(theme.colors.text_muted));
        // `with_size` is the control-size contract used by the desktop shell.
        // A custom-sized icon is painted at 75% of that square, preserving the
        // established 32 px control / 24 px glyph geometry.
        let (control_size, icon_size) = control_geometry(self.content_size);
        let leading = self.leading_element(loader_id, icon_size);
        let aria_label = self.aria_label.or_else(|| self.label.clone());
        let has_text = self.label.is_some() || !self.children.is_empty();
        let padding = if self.flags.contains(ControlFlags::COMPACT) {
            px(4.0)
        } else {
            px(8.0)
        };
        let tooltip = self.tooltip;
        let activation = self.activation;
        let caller_hover_style = self.hover_style;
        let surface = self.surface;
        let base = self.base;
        let caller_style = self.style;

        let control = base
            .occlude()
            .when_some(aria_label, BaseButton::accessibility_label)
            .selected(self.flags.contains(ControlFlags::SELECTED))
            .disabled(self.flags.contains(ControlFlags::DISABLED))
            .tab_index(self.tab_index)
            .tab_stop(
                self.flags.contains(ControlFlags::TAB_STOP)
                    && !self.flags.contains(ControlFlags::DISABLED),
            )
            .aria_selected(self.flags.contains(ControlFlags::SELECTED))
            .flex()
            .flex_shrink_0()
            .relative()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded(px(4.0))
            .font_family(platform_font_family())
            .when(has_text, |this| this.font_weight(control_label_weight()))
            .when(has_text, |this| this.h(control_size).px(padding))
            .when(!has_text, |this| this.size(control_size));
        let control = with_control_surface(control, surface, self.theme.as_ref())
            .when(policy.accepts_input(), gpui::Styled::cursor_pointer)
            .when(!policy.accepts_input(), gpui::Styled::cursor_default);
        let mut control = with_pointer_states(
            control,
            policy,
            caller_hover_style,
            hover_color,
            selected_color,
        )
        .when_some(
            selected_color.filter(|_| self.flags.contains(ControlFlags::SELECTED)),
            gpui::Styled::bg,
        )
        .when(self.flags.contains(ControlFlags::LOADING), |this| {
            this.opacity(0.8)
        })
        .when(self.flags.contains(ControlFlags::DISABLED), |this| {
            this.when_some(disabled_color, gpui::Styled::text_color)
        })
        .focus_visible(move |style| style.border_2().border_color(focus_color))
        .when_some(
            activation.filter(|_| policy.accepts_input()),
            |this, handler| this.on_click(move |event, window, cx| handler(event, window, cx)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap_1()
                .children(leading)
                .children(self.leading)
                .children(self.label)
                .children(self.children)
                .children(self.caret.map(|caret| {
                    caret
                        .with_size((icon_size * 0.75).max(px(10.0)))
                        .into_any_element()
                })),
        );
        control.style().refine(&caller_style);

        if let Some(tooltip) = tooltip {
            let delay = tooltip.delay();
            control
                .tooltip(tooltip.builder())
                .tooltip_show_delay(delay)
                .into_any_element()
        } else {
            control.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use asceify_design_system::AsceifyTheme;
    use gpui::{FontWeight, InteractiveElement, Styled, px};

    use super::super::theme::{ButtonVariant, button_appearance};
    use super::{Control, ControlPolicy, control_geometry, control_label_weight};

    #[test]
    fn ordinary_control_labels_keep_the_platform_normal_weight() {
        assert_eq!(control_label_weight(), FontWeight(500.0));
    }

    #[test]
    fn custom_control_size_preserves_glyph_inset() {
        let (control_size, icon_size) = control_geometry(Some(px(32.0)));
        assert_eq!(control_size, px(32.0));
        assert_eq!(icon_size, px(24.0));

        let (control_size, icon_size) = control_geometry(Some(px(80.0 / 3.0)));
        assert_eq!(control_size, px(80.0 / 3.0));
        assert_eq!(icon_size, px(20.0));
    }

    #[test]
    fn activation_requires_one_enabled_handler() {
        assert!(
            ControlPolicy {
                disabled: false,
                loading: false,
                has_activation: true,
            }
            .accepts_input()
        );
        for policy in [
            ControlPolicy {
                disabled: true,
                loading: false,
                has_activation: true,
            },
            ControlPolicy {
                disabled: false,
                loading: true,
                has_activation: true,
            },
            ControlPolicy {
                disabled: false,
                loading: false,
                has_activation: false,
            },
        ] {
            assert!(!policy.accepts_input());
        }
    }

    #[test]
    fn caller_hover_style_replaces_the_default_control_hover_slot() {
        let control = Control::new("custom_hover").hover(|style| style.opacity(0.5));
        assert!(control.hover_style.is_some());
    }

    #[test]
    fn dialog_actions_use_canonical_primary_and_secondary_fills() {
        let theme = AsceifyTheme::dark();
        let primary = Control::new("primary").variant(&theme, ButtonVariant::Filled);
        let secondary = Control::new("secondary").variant(&theme, ButtonVariant::Secondary);

        assert_eq!(primary.theme, Some(theme));
        assert_eq!(
            primary.surface,
            Some(button_appearance(&theme, ButtonVariant::Filled))
        );
        assert_eq!(primary.resting_fill, Some(theme.colors.button_fill));
        assert_eq!(secondary.theme, Some(theme));
        assert_eq!(
            secondary.surface,
            Some(button_appearance(&theme, ButtonVariant::Secondary))
        );
        assert_eq!(secondary.resting_fill, Some(theme.colors.surface_secondary));
    }
}
